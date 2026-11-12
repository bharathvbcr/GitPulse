//! A read whose callers have all gone is stopped; a read with a caller left is
//! not. Children are real processes the tests start themselves and address by
//! the pid their script writes down; nothing is ever signalled by name.

use super::read_cancel::{self, ReadScope, CANCELLED_MESSAGE};
use super::*;
use crate::ipc_error::IpcErrorKind;
use crate::procguard::LockedSpawn;
use crate::test_support::{coverage_relaxed, process_alive, wait_gone, wait_until};

/// How long a cancelled child may take to leave: the ask, the grace
/// ([`crate::procguard::CHILD_GRACE`]) and a margin, relaxed under coverage.
fn gone_within() -> Duration {
    coverage_relaxed(GONE_WITHIN_BASE)
}

/// [`gone_within`] before relaxing. [`wait_until`] relaxes the limit it is
/// given, so a window that must be exactly `gone_within` is passed as this.
const GONE_WITHIN_BASE: Duration = Duration::from_millis(3_500);

#[test]
fn gone_within_base_is_the_grace_and_its_margin() {
    assert_eq!(
        GONE_WITHIN_BASE,
        crate::procguard::CHILD_GRACE + Duration::from_secs(3)
    );
}

/// A rig read's own deadline, relaxed like the windows the tests watch. A fixed
/// 60 s ran out inside the burst test's "nothing cancelled it" window under
/// coverage (that window is `gone_within`, eight times longer when
/// instrumented): the deadline, not a cancel, ended a child, and the test saw
/// 7 of 8 alive at exactly 60 s. Held to the runner's ceiling: relaxed by load
/// as well (up to 1,920 s), the runner refused every read unrun.
fn read_deadline() -> Duration {
    read_deadline_from(coverage_relaxed(READ_DEADLINE))
}

/// The unrelaxed rig read deadline.
const READ_DEADLINE: Duration = Duration::from_secs(60);

fn read_deadline_from(relaxed: Duration) -> Duration {
    relaxed.min(NETWORK_TIMEOUT)
}

#[test]
fn the_rigs_read_deadline_is_one_the_runner_accepts() {
    use crate::test_support::env::{relaxed_for, MAX_LOAD_FACTOR};
    // Instrumented, at and past the load cap: the worst `coverage_relaxed` gives.
    let cpus = 18;
    let saturated = Some((MAX_LOAD_FACTOR * cpus as f64 * 2.0, cpus));
    let worst = read_deadline_from(relaxed_for(READ_DEADLINE, true, saturated));
    assert!(
        worst <= NETWORK_TIMEOUT,
        "{worst:?} is past the runner's ceiling"
    );
    assert!(
        relaxed_for(READ_DEADLINE, true, saturated) > NETWORK_TIMEOUT,
        "the worst case no longer reaches the ceiling, so this test shows nothing"
    );
    // On this host: never tighter than unrelaxed, and longer than the burst
    // test's "nothing cancelled it" window, which its children must outlive.
    let here = read_deadline();
    assert!(
        here >= READ_DEADLINE,
        "{here:?} is tighter than the unrelaxed read"
    );
    assert!(
        here > gone_within(),
        "{here:?} ends inside the {:?} window",
        gone_within()
    );
}

/// The shell fragment a child runs to record itself: its pid and its own start
/// time, as `ps` reports it, on one line. Written by the child, so the start
/// time is the child's and not whichever process holds the pid by the time the
/// test looks.
const RECORD_SELF: &str = r#"echo "$$ $(ps -o lstart= -p $$)""#;

/// What `ps` says about when `pid` started, if it is running.
fn started_at(pid: u32) -> Option<String> {
    let out = Command::new("ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .output_locked()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !text.is_empty()).then_some(text)
}

/// The `(pid, start time)` pairs a test's children have written to `file` so far.
fn recorded_children(file: &Path) -> Vec<(u32, String)> {
    std::fs::read_to_string(file)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let (pid, stamp) = line.trim().split_once(' ')?;
            Some((pid.parse().ok()?, stamp.trim().to_string()))
        })
        .collect()
}

/// The pids a test's scripts have written to `file` so far.
fn recorded(file: &Path) -> Vec<u32> {
    recorded_children(file)
        .into_iter()
        .map(|(pid, _)| pid)
        .collect()
}

/// Kills, at the end of the test, any child the code under test left running.
/// A child is only ever signalled if it is still the process that recorded
/// itself: its start time has to match the one it wrote down, so a pid that was
/// reused by a stranger is left alone.
struct Reaper(PathBuf);

impl Drop for Reaper {
    fn drop(&mut self) {
        for (pid, stamp) in recorded_children(&self.0) {
            if started_at(pid).as_deref() != Some(stamp.as_str()) {
                continue;
            }
            if let Ok(pid) = i32::try_from(pid) {
                // SAFETY: the process that wrote this pid and this start time.
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
    }
}

struct Rig {
    gate: &'static SpawnGate,
    // Fields drop in declaration order. The reaper reads the pids file, which
    // lives in `_dir`, so it has to run first: declared after `_dir` it found
    // the directory already gone, killed nothing, and every child a test left
    // running lived on until its read's own deadline.
    _reaper: Reaper,
    _dir: tempfile::TempDir,
    cwd: PathBuf,
    pids: PathBuf,
}

impl Rig {
    fn new(slots: usize) -> Self {
        let dir = tempfile::TempDir::new().unwrap();
        let cwd = dir.path().canonicalize().unwrap();
        let pids = cwd.join("pids");
        Self {
            gate: Box::leak(Box::new(SpawnGate::new(slots))),
            _reaper: Reaper(pids.clone()),
            _dir: dir,
            cwd,
            pids,
        }
    }

    /// `tag` makes the argv, so the sharing key, differ between flights.
    fn script(&self, tag: &str, body: &str) -> String {
        format!(
            ": {tag}; {RECORD_SELF} >> '{}'; {body}",
            self.pids.display()
        )
    }

    fn read(
        &self,
        id: Option<&str>,
        script: String,
    ) -> thread::JoinHandle<Result<BoundedRun, CommandError>> {
        let (gate, cwd, id) = (self.gate, self.cwd.clone(), id.map(str::to_string));
        thread::spawn(move || {
            let scope = ReadScope::open(id.as_deref());
            let _in = scope.enter();
            let mut cmd = Command::new("/bin/sh");
            cmd.args(["-c", script.as_str()]).current_dir(&cwd);
            run_read_shared(cmd, "cancel-test", read_deadline(), 4096, gate)
        })
    }

    fn started(&self) -> Vec<u32> {
        recorded(&self.pids)
    }

    /// [`Self::read`] for the request that must lead its flight: returns once
    /// the flight is joinable, so a reader started after it is a joiner.
    fn lead(
        &self,
        id: Option<&str>,
        script: String,
    ) -> thread::JoinHandle<Result<BoundedRun, CommandError>> {
        let reader = self.read(id, script);
        assert!(
            wait_until(Duration::from_secs(10), || self.flight_queued()),
            "the leader never queued"
        );
        reader
    }

    /// Whether a flight for this rig's directory is in the shared table.
    fn flight_queued(&self) -> bool {
        super::shared_reads()
            .lock()
            .unwrap()
            .keys()
            .any(|key| key.cwd == self.cwd)
    }

    fn wait_started(&self, count: usize) -> Vec<u32> {
        assert!(
            wait_until(Duration::from_secs(10), || self.started().len() >= count),
            "only {} child(ren) started",
            self.started().len()
        );
        self.started()
    }

    fn wait_joined(&self, joiners: u64) {
        assert!(
            wait_until(Duration::from_secs(10), || {
                self.gate.counters()[Admission::Reactive.index()].coalesced >= joiners
            }),
            "the readers never queued together"
        );
    }
}

fn text(run: &BoundedRun) -> String {
    String::from_utf8_lossy(&run.stdout).trim().to_string()
}

/// A sole waiter's read is cancelled: the child it started is gone within the
/// grace, and what comes back is a cancelled run, not output.
#[cfg(unix)]
#[test]
fn a_sole_waiters_read_is_cancelled_and_its_child_is_killed() {
    let rig = Rig::new(4);
    let reader = rig.read(Some("rc-sole"), rig.script("sole", "exec sleep 60"));
    let pid = rig.wait_started(1)[0];
    let asked = Instant::now();
    assert!(read_cancel::cancel("rc-sole"), "a live read is signalled");
    let run = reader.join().unwrap().expect("a cancelled run is returned");
    assert!(run.cancelled && !run.success);
    assert!(wait_gone(pid, gone_within()), "pid {pid} still running");
    assert!(
        asked.elapsed() < gone_within(),
        "the cancel took {:?}",
        asked.elapsed()
    );
}

/// Through the real runner: the cancelled read is an error, so no caller
/// parses its partial output, and `ref_cache` has nothing to keep.
#[cfg(unix)]
#[test]
fn a_cancelled_git_read_is_an_error_and_never_an_answer() {
    for (shared, id) in [(true, "rc-git-shared"), (false, "rc-git-own")] {
        let repo = crate::test_support::git_repo();
        let dir = tempfile::TempDir::new().unwrap();
        let pids = dir.path().join("pids");
        let _reaper = Reaper(pids.clone());
        // An alias that runs a shell: a slow git command the test controls.
        let alias = format!(
            "alias.rcslow=!{RECORD_SELF} >> '{}'; echo partial; exec sleep 60",
            pids.display()
        );
        let path = repo.path().to_path_buf();
        let reader = thread::spawn(move || {
            let scope = ReadScope::open(Some(id));
            let _in = scope.enter();
            let args = ["-c", alias.as_str(), "rcslow"];
            if shared {
                git_text_shared(&path, &args).map(|text| (text, None))
            } else {
                git_text_capped(&path, &args, 4096)
            }
        });
        assert!(
            wait_until(Duration::from_secs(10), || !recorded(&pids).is_empty()),
            "the git child never started"
        );
        let pid = recorded(&pids)[0];
        assert!(read_cancel::cancel(id));
        let outcome = reader.join().unwrap();
        let error = outcome.expect_err("a cancelled read must not return its partial output");
        assert_eq!(error.message(), CANCELLED_MESSAGE, "shared={shared}");
        assert_eq!(error.kind(), IpcErrorKind::ReadCancelled, "shared={shared}");
        assert!(wait_gone(pid, gone_within()), "shared={shared}: pid {pid}");
    }
}

/// A fallback after a failed read must not spawn for a caller who has gone.
#[cfg(unix)]
#[test]
fn a_cancelled_request_starts_no_further_child() {
    let repo = crate::test_support::git_repo();
    let scope = ReadScope::open(Some("rc-no-more"));
    let _in = scope.enter();
    assert!(read_cancel::cancel("rc-no-more"));
    let (outcome, spawned) =
        spawn_labels_during(|| git_text(repo.path(), &["status", "--porcelain"]));
    let refused = outcome.unwrap_err();
    assert_eq!(refused.message(), CANCELLED_MESSAGE);
    assert_eq!(refused.kind(), IpcErrorKind::ReadCancelled);
    assert!(
        spawned.is_empty(),
        "spawned for a cancelled request: {spawned:?}"
    );
}

/// Two requests wait on one child. One of them leaving does not stop it, and
/// both get the answer (or, for the one that left, a prompt cancel).
#[cfg(unix)]
#[test]
fn a_read_with_another_waiter_is_not_cancelled_by_the_leader_leaving() {
    let rig = Rig::new(1);
    let held = rig
        .gate
        .acquire(Instant::now() + Duration::from_secs(5))
        .unwrap();
    let script = rig.script("two", "sleep 0.6; echo answer");
    let leader = rig.lead(Some("rc-lead"), script.clone());
    let joiner = rig.read(Some("rc-join"), script);
    rig.wait_joined(1);
    assert!(read_cancel::cancel("rc-lead"));
    drop(held);
    let led = leader
        .join()
        .unwrap()
        .expect("the leader's caller still gets the answer");
    let joined = joiner.join().unwrap().expect("the joiner gets the answer");
    assert!(!led.cancelled && !joined.cancelled);
    assert_eq!(
        (text(&led), text(&joined)),
        ("answer".into(), "answer".into())
    );
    assert_eq!(rig.started().len(), 1, "one child serves both");
}

/// The joiner leaves: it returns at once with the cancellation, the leader's
/// child runs on and the leader's caller gets the answer.
#[cfg(unix)]
#[test]
fn a_joiner_leaving_returns_promptly_and_the_leader_is_untouched() {
    // A joiner reaches the leader's flight only while it is queued: hold the
    // gate's one slot.
    let rig = Rig::new(1);
    let held = rig
        .gate
        .acquire(Instant::now() + Duration::from_secs(5))
        .unwrap();
    let script = rig.script("joiner", "sleep 1.5; echo answer");
    let leader = rig.lead(Some("rc-l2"), script.clone());
    let joiner = rig.read(Some("rc-j2"), script);
    rig.wait_joined(1);
    let left = Instant::now();
    assert!(read_cancel::cancel("rc-j2"));
    let error = joiner.join().unwrap().expect_err("the joiner left");
    assert_eq!(error.message(), CANCELLED_MESSAGE);
    assert_eq!(error.kind(), IpcErrorKind::ReadCancelled);
    assert!(left.elapsed() < gone_within(), "{:?}", left.elapsed());
    drop(held);
    let answer = leader.join().unwrap().expect("the leader is untouched");
    assert_eq!(text(&answer), "answer");
    assert!(!answer.cancelled);
}

/// Every waiter gone while the read is still queued: the flight leaves the
/// table, and a caller that arrives afterwards starts a fresh read that
/// completes. It does not inherit the cancellation.
#[cfg(unix)]
#[test]
fn a_late_joiner_of_a_cancelled_flight_gets_a_fresh_complete_read() {
    let rig = Rig::new(1);
    let held = rig
        .gate
        .acquire(Instant::now() + Duration::from_secs(5))
        .unwrap();
    let script = rig.script("late", "echo fresh");
    let first = rig.lead(Some("rc-late-1"), script.clone());
    let second = rig.read(Some("rc-late-2"), script.clone());
    rig.wait_joined(1);
    assert!(read_cancel::cancel("rc-late-1"));
    assert!(read_cancel::cancel("rc-late-2"));
    // The decision is made by the leader's poll while it waits in the queue.
    assert!(
        wait_until(Duration::from_secs(10), || !rig.flight_queued()),
        "the cancelled flight stayed joinable"
    );
    let late = rig.read(None, script);
    drop(held);
    let fresh = late.join().unwrap().expect("a fresh read");
    assert_eq!(text(&fresh), "fresh");
    assert!(!fresh.cancelled);
    assert!(
        first.join().unwrap().is_err(),
        "the cancelled leader got no answer"
    );
    assert!(
        second.join().unwrap().is_err(),
        "the cancelled joiner got no answer"
    );
    assert_eq!(
        rig.started().len(),
        1,
        "only the fresh read started a child"
    );
}

/// A read that never named an id is a waiter that never leaves.
#[cfg(unix)]
#[test]
fn a_waiter_without_an_id_keeps_the_read_alive() {
    let rig = Rig::new(1);
    let held = rig
        .gate
        .acquire(Instant::now() + Duration::from_secs(5))
        .unwrap();
    let script = rig.script("plain", "sleep 0.3; echo answer");
    let named = rig.lead(Some("rc-named"), script.clone());
    let plain = rig.read(None, script);
    rig.wait_joined(1);
    assert!(read_cancel::cancel("rc-named"));
    drop(held);
    assert_eq!(text(&plain.join().unwrap().unwrap()), "answer");
    drop(named);
    assert_eq!(rig.started().len(), 1);
}

/// A cancel that arrives after the read ended, or races its end, is a no-op:
/// nothing fails, and the next read, under the same id, is whole.
#[cfg(unix)]
#[test]
fn a_cancel_racing_the_reads_end_loses_cleanly() {
    let rig = Rig::new(4);
    for round in 0..40 {
        let id = format!("rc-race-{round}");
        let reader = rig.read(Some(&id), rig.script(&format!("race{round}"), "echo ok"));
        let canceller = {
            let id = id.clone();
            thread::spawn(move || {
                for _ in 0..200 {
                    read_cancel::cancel(&id);
                    thread::yield_now();
                }
            })
        };
        let outcome = reader.join().unwrap();
        canceller.join().unwrap();
        match outcome {
            Ok(run) if run.cancelled => {}
            Ok(run) => assert_eq!(text(&run), "ok", "round {round}"),
            Err(error) => assert!(
                error.message().contains("cancelled"),
                "round {round}: {}",
                error.message()
            ),
        }
        assert!(
            !read_cancel::cancel(&id),
            "a finished read is not registered"
        );
        let again = rig.read(Some(&id), rig.script(&format!("after{round}"), "echo ok"));
        assert_eq!(text(&again.join().unwrap().unwrap()), "ok", "round {round}");
    }
}

/// A burst of superseding clicks on a slow read: each click starts the read
/// for its commit and gives up the previous one. Counter-based, so load does
/// not matter: how many children are still running once the last click has
/// settled, and how much child time was spent. The "before" arm names no ids,
/// which is today's behaviour exactly.
#[cfg(unix)]
#[test]
fn a_burst_of_superseded_clicks_leaves_one_child_running() {
    const CLICKS: usize = 8;
    // An uncancelled child must outlive the whole burst plus the window the
    // test watches it for. Both stretch with host load (`gone_within` is
    // relaxed by up to 4x, and each spawn records itself through `ps`), so a
    // fixed `sleep 20` ran out inside the window on a busy host and the
    // "nothing cancelled it" arm saw a child leave on its own. The children
    // are reaped by pid when the rig drops, so a long life costs nothing.
    let lifetime = (gone_within() * 4 + Duration::from_secs(120)).as_secs();
    let arm = |cancellable: bool| {
        let rig = Rig::new(CLICKS + 2);
        let began = Instant::now();
        let mut readers = Vec::new();
        for click in 0..CLICKS {
            let id = format!("rc-burst-{cancellable}-{click}");
            readers.push(rig.read(
                cancellable.then_some(id.as_str()),
                rig.script(&format!("click{click}"), &format!("exec sleep {lifetime}")),
            ));
            rig.wait_started(click + 1);
            if click > 0 && cancellable {
                read_cancel::cancel(&format!("rc-burst-{cancellable}-{}", click - 1));
            }
        }
        // The last click has settled: give the cancelled children their grace.
        // Both arms watch exactly `gone_within`: `wait_until` relaxes what it is
        // given, so it gets the base. Given `gone_within()` it relaxed twice
        // (about 224 s at no load under coverage, growing with the square of
        // the load factor) and outran `lifetime`, which grows linearly: the
        // children ended on their own and the uncancelled arm saw 7 alive.
        let alive_now = || {
            rig.started()
                .iter()
                .filter(|pid| process_alive(**pid))
                .count()
        };
        if cancellable {
            assert!(
                wait_until(GONE_WITHIN_BASE, || alive_now() <= 1),
                "the cancelled children were not gone within the grace: {} alive",
                alive_now()
            );
        } else {
            // Nothing was cancelled, so the children must still all be running
            // after the same window: that the condition "fewer are alive" never
            // holds for the whole of it is what is being shown.
            assert!(
                !wait_until(GONE_WITHIN_BASE, || alive_now() < CLICKS),
                "a child exited although nothing cancelled it: {} alive",
                alive_now()
            );
        }
        let pids = rig.started();
        let alive = pids.iter().filter(|pid| process_alive(**pid)).count();
        let child_seconds = began.elapsed().as_secs_f64() * alive as f64;
        eprintln!(
            "burst cancellable={cancellable}: {alive}/{CLICKS} children running after the last click, ~{child_seconds:.1} child-seconds still being spent"
        );
        // The rest are stopped by pid (these children are this test's own),
        // then the readers are collected.
        drop(rig);
        for reader in readers {
            let _ = reader.join();
        }
        alive
    };
    let before = arm(false);
    let after = arm(true);
    assert_eq!(before, CLICKS, "without ids every superseded child runs on");
    assert_eq!(after, 1, "with ids only the last click's child runs");
}

/// A read cancelled while it waits for a slot is the cancellation a mid-read
/// cancel is, and no fault of the process: the failure counter, which feeds
/// health reporting, does not move.
#[cfg(unix)]
#[test]
fn a_read_cancelled_while_it_queues_is_a_cancellation_and_not_a_process_failure() {
    let rig = Rig::new(1);
    let held = rig
        .gate
        .acquire(Instant::now() + Duration::from_secs(5))
        .unwrap();
    let scope = ReadScope::open(Some("rc-queued"));
    let _in = scope.enter();
    let cwd = rig.cwd.clone();
    let canceller = thread::spawn(move || {
        assert!(
            wait_until(Duration::from_secs(10), || {
                super::shared_reads()
                    .lock()
                    .unwrap()
                    .keys()
                    .any(|key| key.cwd == cwd)
            }),
            "the read never queued"
        );
        assert!(read_cancel::cancel("rc-queued"));
    });
    let before = PROCESS_FAILURES.get();
    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", rig.script("queued", "echo ran").as_str()])
        .current_dir(&rig.cwd);
    let outcome = run_read_shared(cmd, "queued-test", read_deadline(), 4096, rig.gate);
    canceller.join().unwrap();
    let error = outcome.expect_err("a cancelled read does not run");
    assert_eq!(
        error.message(),
        CANCELLED_MESSAGE,
        "the same cancellation as one cancelled while it ran"
    );
    assert_eq!(error.kind(), IpcErrorKind::ReadCancelled);
    assert_eq!(
        PROCESS_FAILURES.get(),
        before,
        "a cancel was counted as a failure"
    );
    assert!(rig.started().is_empty(), "nothing was spawned");
    drop(held);
}

/// The cancellation is recognised by its kind, and nothing else is: text that
/// says the same words, as git's stderr can, is not a cancel.
#[test]
fn only_a_cancellation_is_exempt_from_the_failure_count() {
    let cancelled = read_cancel::cancelled();
    assert!(is_cancellation(&cancelled));
    assert_eq!(cancelled.kind(), IpcErrorKind::ReadCancelled);
    // A layer that puts its own words round the cancel leaves it a cancel.
    let wrapped = read_cancel::cancelled().wrap(|text| format!("graph window: {text}"));
    assert!(is_cancellation(&wrapped));
    for other in [
        "git log timed out after 90s",
        "Failed to spawn git log: forced by test",
        "git log exited with status 128",
        CANCELLED_MESSAGE,
        "git log cancelled before spawn",
    ] {
        assert!(
            !is_cancellation(&CommandError::from(other)),
            "text alone is not a cancel: {other}"
        );
    }
    // The same words from a process's stderr are a process failure.
    let stderr = CommandError::from(format!("git log failed: {CANCELLED_MESSAGE}"));
    assert!(!is_cancellation(&stderr));
}

/// Only a read-cancel observer's cancel is a `ReadCancelled`. A secrets, hooks,
/// tool-install, devmap or commit-graph cancel keeps the wire kind it has on
/// main (unclassified), whether it is counted as a failure or not.
#[test]
fn another_observers_cancel_keeps_its_wire_kind() {
    struct SomeoneElse;
    impl ProcessObserver for SomeoneElse {
        fn cancelled(&self) -> bool {
            true
        }
    }
    assert!(SomeoneElse.cancellation().is_none());
    assert!(().cancellation().is_none());
    let refused = refusal_message("kingfisher", Refusal::Cancelled);
    assert!(refused.message().ends_with(CANCELLED_BEFORE_SPAWN));
    assert_eq!(refused.kind(), IpcErrorKind::Unclassified);
    assert!(!is_cancellation(&refused));
    // The two that are read-cancel observers say so.
    let flag = read_cancel::ReadCancel::new();
    assert_eq!(
        read_cancel::OwnCancel(&flag)
            .cancellation()
            .map(|e| e.kind()),
        Some(IpcErrorKind::ReadCancelled)
    );
}

/// A history window is read as one long walk cut into chunks. Cancelled at that
/// walk, it keeps nothing in `ref_cache` and leaves no state for the next
/// call: a later call walks again and returns the whole window.
#[cfg(unix)]
#[test]
fn a_cancelled_history_read_ahead_keeps_nothing_and_leaks_nothing_into_the_next_call() {
    use crate::engine::git_reader::{with_history_chunk, GitReader};
    use crate::graph::RefScope;
    let repo = crate::test_support::git_repo();
    for n in 0..12 {
        crate::test_support::git_in(
            repo.path(),
            &["commit", "--allow-empty", "-q", "-m", &format!("c{n}")],
        );
    }
    let path = repo.path().to_str().unwrap().to_string();
    let window =
        |path: &str| GitReader::read_commit_history_paged(path, 0, 10, None, None, RefScope::Named);
    crate::engine::ref_cache::eager(|| {
        with_history_chunk(3, || {
            // The window's first walk is cancelled as it starts, by the caller
            // leaving: the hook is the caller's abort landing at that spawn.
            let scope = ReadScope::open(Some("rc-readahead"));
            let outcome = {
                let _in = scope.enter();
                with_forced_spawn_failure_then(
                    || {
                        read_cancel::cancel("rc-readahead");
                    },
                    || window(&path),
                )
            };
            assert!(
                outcome.is_err(),
                "the cancelled window must not return rows"
            );
            drop(scope);

            // Nothing was kept: the same window, asked again, walks.
            let (again, walks) = spawn_labels_during(|| window(&path));
            let rows = again.expect("a later call is a fresh read");
            assert_eq!(rows.len(), 10, "the whole window comes back");
            assert!(
                walks.iter().any(|label| label == "git log"),
                "an answer was kept from the cancelled read: {walks:?}"
            );

            // And what that call kept is whole: asked a third time, it is served.
            let (third, walks) = spawn_labels_during(|| window(&path));
            assert_eq!(third.unwrap().len(), rows.len());
            assert!(
                !walks.iter().any(|label| label == "git log"),
                "the control: a complete window is kept ({walks:?})"
            );
        })
    });
}
