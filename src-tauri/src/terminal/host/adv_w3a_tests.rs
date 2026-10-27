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

/// Rework round 1, fix (a): `record_exit` is once per session by construction.
/// Two reporters of one exit race: the first is held inside its run's
/// bookkeeping (`finished`), the second runs to the end meanwhile. The second
/// must not run the bookkeeping or announce the exit again.
#[test]
fn two_concurrent_record_exits_for_one_session_run_finished_once() {
    let fake = FakeHost::start(&[]);
    let app = fixture(fake.config());
    let host = the_client(&app);
    let log = Arc::new(ExitLog::default());
    host.set_events(log.clone());
    let observer = SlowFinish::new();
    lock(&host.inner.proxies).insert(
        "term-9-twice".into(),
        Proxy::new(nowhere(), Some(observer.clone()), None, false, false),
    );
    let first = {
        let inner = host.inner.clone();
        std::thread::spawn(move || inner.record_exit(exit_of("term-9-twice", Some(0))))
    };
    until(
        "the first exit's bookkeeping",
        Duration::from_secs(5),
        || observer.started.load(Ordering::Acquire) >= 1,
    );

    host.inner.record_exit(exit_of("term-9-twice", Some(0)));
    observer.open.store(true, Ordering::Release);
    first.join().unwrap();

    let exits = log.exits.lock().unwrap().clone();
    assert_eq!(
        observer.started.load(Ordering::Acquire),
        1,
        "the run's bookkeeping ran more than once for one session: {exits:?}"
    );
    assert_eq!(
        exits.iter().filter(|(id, _)| id == "term-9-twice").count(),
        1,
        "one session, one exit announcement: {exits:?}"
    );
    assert!(!lock(&host.inner.proxies).contains_key("term-9-twice"));
}

/// Rework round 2: a second report that arrives while the first is still
/// recording touches neither the proxy nor the host. The first is held inside
/// `finished` (the run's receipt is not written yet); the second runs to the
/// end. The host must not be told to forget the exit, or an app that dies
/// meanwhile has lost it for good, and the session must still be there until
/// the exit is announced. Once the first finishes, the host is told exactly
/// once, and only then.
///
/// The host's `Forget` is observed by the fake host's counter (`Shared.forgets`
/// in `client_adversarial_tests.rs`), the narrowest view of it the test
/// connection has.
#[test]
fn a_second_report_during_the_recording_leaves_the_proxy_and_the_host_alone() {
    let fake = FakeHost::start(&[]);
    let app = fixture(fake.config());
    let host = the_client(&app);
    let log = Arc::new(ExitLog::default());
    host.set_events(log.clone());
    app.sessions.connect_host().unwrap();
    let observer = SlowFinish::new();
    lock(&host.inner.proxies).insert(
        "term-9-early".into(),
        Proxy::new(nowhere(), Some(observer.clone()), None, false, false),
    );
    let first = {
        let inner = host.inner.clone();
        std::thread::spawn(move || inner.record_exit(exit_of("term-9-early", Some(0))))
    };
    until(
        "the first exit's bookkeeping",
        Duration::from_secs(5),
        || observer.started.load(Ordering::Acquire) >= 1,
    );

    host.inner.record_exit(exit_of("term-9-early", Some(0)));
    std::thread::sleep(Duration::from_millis(200));

    assert!(
        lock(&host.inner.proxies).contains_key("term-9-early"),
        "the session was removed before its exit was recorded"
    );
    assert_eq!(
        fake.shared.forgets.load(Ordering::Acquire),
        0,
        "the host was told to forget an exit whose receipt is not written"
    );

    observer.open.store(true, Ordering::Release);
    first.join().unwrap();
    until("the host told to forget", Duration::from_secs(5), || {
        fake.shared.forgets.load(Ordering::Acquire) >= 1
    });
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(fake.shared.forgets.load(Ordering::Acquire), 1);
    assert!(!lock(&host.inner.proxies).contains_key("term-9-early"));
    assert_eq!(log.exits.lock().unwrap().len(), 1);
}

/// W4c, ft-f70ee2bb item 3 (coverage; it passes on the lane tip, where the
/// edge was argued and not run). `ended` is bounded at `REMEMBERED_EXITS`, but
/// only recorded ids live in it: an exit still in flight is held in `recording`
/// and cannot be pushed out by the exits that finish around it. So a session
/// whose exit is mid-recording while more than `REMEMBERED_EXITS` others are
/// recorded is still recognised when it is reported again: the late report
/// returns at once, the bookkeeping runs once, and when the first reporter
/// finishes the id enters `ended` as its newest entry.
#[test]
fn an_exit_in_flight_survives_the_wrap_of_ended_and_is_recorded_once() {
    let fake = FakeHost::start(&[]);
    let app = fixture(fake.config());
    let host = the_client(&app);
    let log = Arc::new(ExitLog::default());
    host.set_events(log.clone());
    let observer = SlowFinish::new();
    lock(&host.inner.proxies).insert(
        "term-9-wrap".into(),
        Proxy::new(nowhere(), Some(observer.clone()), None, false, false),
    );
    let first = {
        let inner = host.inner.clone();
        std::thread::spawn(move || inner.record_exit(exit_of("term-9-wrap", Some(0))))
    };
    until(
        "the in-flight exit's bookkeeping",
        Duration::from_secs(5),
        || observer.started.load(Ordering::Acquire) >= 1,
    );

    let others = REMEMBERED_EXITS + 5;
    for n in 0..others {
        host.inner
            .record_exit(exit_of(&format!("term-9-fill-{n}"), Some(0)));
    }
    {
        let ended = lock(&host.inner.ended);
        assert_eq!(ended.len(), REMEMBERED_EXITS, "ended is bounded");
        assert!(
            ended.iter().all(|id| id != "term-9-fill-0"),
            "the oldest recorded exit was pushed out"
        );
        assert!(
            ended.iter().all(|id| id != "term-9-wrap"),
            "an in-flight exit is not in ended yet"
        );
    }
    assert!(
        lock(&host.inner.recording).contains("term-9-wrap"),
        "the wrap forgot an exit that was still being recorded"
    );

    host.inner.record_exit(exit_of("term-9-wrap", Some(0)));
    assert_eq!(
        observer.started.load(Ordering::Acquire),
        1,
        "a report during the wrap ran the bookkeeping again"
    );
    assert!(
        !log.ended("term-9-wrap"),
        "announced before it was recorded"
    );

    observer.open.store(true, Ordering::Release);
    first.join().unwrap();
    assert!(lock(&host.inner.recording).is_empty());
    {
        let ended = lock(&host.inner.ended);
        assert_eq!(ended.len(), REMEMBERED_EXITS);
        assert_eq!(ended.back().map(String::as_str), Some("term-9-wrap"));
    }

    host.inner.record_exit(exit_of("term-9-wrap", Some(0)));
    let exits = log.exits.lock().unwrap().clone();
    assert_eq!(
        exits.iter().filter(|(id, _)| id == "term-9-wrap").count(),
        1,
        "one session, one exit announcement: {exits:?}"
    );
    assert_eq!(observer.started.load(Ordering::Acquire), 1);
    assert_eq!(exits.len(), others + 1);
}

/// Session events whose first `exit` panics, after the run's bookkeeping and
/// outside it: the part of `record_exit` that `catch_unwind` around `finished`
/// does not cover.
struct PanicsOnce {
    armed: AtomicBool,
    log: ExitLog,
}

impl SessionEvents for PanicsOnce {
    fn loss(&self, _: TerminalLossPayload) {}
    fn notice(&self, _: crate::alerts::Notice) {}
    fn scan_drops(&self, _: u64) {}
    fn exit(&self, ended: SessionEnded) {
        if self.armed.swap(false, Ordering::AcqRel) {
            panic!("the exit sink panicked");
        }
        self.log.exit(ended);
    }
}

/// W4c, ft-f70ee2bb item 3 (coverage). A panic in `record_exit` outside
/// `finished` (here, in the events sink) unwinds through the `Recording` guard,
/// whose `Drop` still moves the id from `recording` to `ended`. Without that
/// the id would stay in flight for good and every later report of it would
/// return as "being recorded now", never clearing the host's copy. After the
/// panic a repeat report is the post-recording kind: no second bookkeeping,
/// no announcement, and the proxy stays gone.
#[test]
fn a_panic_in_record_exit_outside_finished_still_ends_the_claim() {
    let fake = FakeHost::start(&[]);
    let app = fixture(fake.config());
    let host = the_client(&app);
    let events = Arc::new(PanicsOnce {
        armed: AtomicBool::new(true),
        log: ExitLog::default(),
    });
    host.set_events(events.clone());
    app.sessions.connect_host().unwrap();
    let observer = SlowFinish::new();
    observer.open.store(true, Ordering::Release);
    lock(&host.inner.proxies).insert(
        "term-9-boom".into(),
        Proxy::new(nowhere(), Some(observer.clone()), None, false, false),
    );

    let caught = catch_unwind(AssertUnwindSafe(|| {
        host.inner.record_exit(exit_of("term-9-boom", Some(0)))
    }));

    assert!(caught.is_err(), "the sink's panic was swallowed");
    assert!(
        lock(&host.inner.recording).is_empty(),
        "the claim outlived the panic"
    );
    assert!(
        lock(&host.inner.ended).iter().any(|id| id == "term-9-boom"),
        "the id was not recorded as ended"
    );
    assert_eq!(observer.started.load(Ordering::Acquire), 1);
    assert!(!lock(&host.inner.proxies).contains_key("term-9-boom"));
    assert_eq!(
        fake.shared.forgets.load(Ordering::Acquire),
        0,
        "the host was told to forget an exit that was never announced"
    );

    host.inner.record_exit(exit_of("term-9-boom", Some(0)));
    until(
        "the repeat report clears the host's copy",
        Duration::from_secs(5),
        || fake.shared.forgets.load(Ordering::Acquire) >= 1,
    );
    assert_eq!(
        observer.started.load(Ordering::Acquire),
        1,
        "the bookkeeping ran again for a recorded exit"
    );
    assert!(
        events.log.exits.lock().unwrap().is_empty(),
        "an exit whose announcement panicked was announced by the repeat"
    );
}

/// A report that arrives after the exit is recorded keeps today's behaviour:
/// it clears the host's copy again and removes any proxy, and announces nothing.
#[test]
fn a_second_report_after_the_recording_only_clears_the_hosts_copy() {
    let fake = FakeHost::start(&[]);
    let app = fixture(fake.config());
    let host = the_client(&app);
    let log = Arc::new(ExitLog::default());
    host.set_events(log.clone());
    app.sessions.connect_host().unwrap();
    host.inner.record_exit(exit_of("term-9-late", Some(0)));
    until("the first forget", Duration::from_secs(5), || {
        fake.shared.forgets.load(Ordering::Acquire) >= 1
    });
    lock(&host.inner.proxies).insert(
        "term-9-late".into(),
        Proxy::new(nowhere(), None, None, false, false),
    );

    host.inner.record_exit(exit_of("term-9-late", Some(0)));

    until("the second forget", Duration::from_secs(5), || {
        fake.shared.forgets.load(Ordering::Acquire) >= 2
    });
    assert!(!lock(&host.inner.proxies).contains_key("term-9-late"));
    assert_eq!(log.exits.lock().unwrap().len(), 1, "announced once");
}
