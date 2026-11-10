//! A read whose callers have all gone is stopped; a read with a caller left is
//! not. Children are real processes the tests start themselves and address by
//! the pid their script writes down; nothing is ever signalled by name.

use super::read_cancel::{self, ReadScope, CANCELLED_MESSAGE};
use super::*;
use crate::test_support::{coverage_relaxed, process_alive, wait_gone};

/// How long a cancelled child may take to leave: the ask, the grace
/// ([`crate::procguard::CHILD_GRACE`]) and a margin, relaxed under coverage.
fn gone_within() -> Duration {
    coverage_relaxed(crate::procguard::CHILD_GRACE + Duration::from_secs(3))
}

/// Every pid the scripts wrote down, killed at the end of the test if the code
/// under test left it running. Only pids this test's own children recorded.
struct Reaper(PathBuf);

/// The pids a test's scripts have written to `file` so far.
fn recorded(file: &Path) -> Vec<u32> {
    std::fs::read_to_string(file)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.trim().parse().ok())
        .collect()
}

impl Drop for Reaper {
    fn drop(&mut self) {
        for pid in recorded(&self.0) {
            if let (true, Ok(pid)) = (process_alive(pid), i32::try_from(pid)) {
                // SAFETY: a pid that a child of this test wrote down itself.
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
    }
}

struct Rig {
    gate: &'static SpawnGate,
    _dir: tempfile::TempDir,
    cwd: PathBuf,
    pids: PathBuf,
    _reaper: Reaper,
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
        format!(": {tag}; echo $$ >> '{}'; {body}", self.pids.display())
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
            run_read_shared(cmd, "cancel-test", Duration::from_secs(60), 4096, gate)
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
        let until = Instant::now() + coverage_relaxed(Duration::from_secs(10));
        while !super::shared_reads()
            .lock()
            .unwrap()
            .keys()
            .any(|key| key.cwd == self.cwd)
        {
            assert!(Instant::now() < until, "the leader never queued");
            thread::sleep(Duration::from_millis(5));
        }
        reader
    }

    fn wait_started(&self, count: usize) -> Vec<u32> {
        let until = Instant::now() + coverage_relaxed(Duration::from_secs(10));
        loop {
            let pids = self.started();
            if pids.len() >= count {
                return pids;
            }
            assert!(Instant::now() < until, "only {} child(ren) started", pids.len());
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_joined(&self, joiners: u64) {
        let until = Instant::now() + coverage_relaxed(Duration::from_secs(10));
        while self.gate.counters()[Admission::Reactive.index()].coalesced < joiners {
            assert!(Instant::now() < until, "the readers never queued together");
            thread::sleep(Duration::from_millis(5));
        }
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
    let reader = rig.read(
        Some("rc-sole"),
        rig.script("sole", "exec sleep 60"),
    );
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
            "alias.rcslow=!echo $$ >> '{}'; echo partial; exec sleep 60",
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
        let until = Instant::now() + coverage_relaxed(Duration::from_secs(10));
        while recorded(&pids).is_empty() {
            assert!(Instant::now() < until, "the git child never started");
            thread::sleep(Duration::from_millis(10));
        }
        let pid = recorded(&pids)[0];
        assert!(read_cancel::cancel(id));
        let outcome = reader.join().unwrap();
        let error = outcome.expect_err("a cancelled read must not return its partial output");
        assert_eq!(error.message(), CANCELLED_MESSAGE, "shared={shared}");
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
    let (outcome, spawned) = spawn_labels_during(|| git_text(repo.path(), &["status", "--porcelain"]));
    assert_eq!(outcome.unwrap_err().message(), CANCELLED_MESSAGE);
    assert!(spawned.is_empty(), "spawned for a cancelled request: {spawned:?}");
}

/// Two requests wait on one child. One of them leaving does not stop it, and
/// both get the answer (or, for the one that left, a prompt cancel).
#[cfg(unix)]
#[test]
fn a_read_with_another_waiter_is_not_cancelled_by_the_leader_leaving() {
    let rig = Rig::new(1);
    let held = rig.gate.acquire(Instant::now() + Duration::from_secs(5)).unwrap();
    let script = rig.script("two", "sleep 0.6; echo answer");
    let leader = rig.lead(Some("rc-lead"), script.clone());
    let joiner = rig.read(Some("rc-join"), script);
    rig.wait_joined(1);
    assert!(read_cancel::cancel("rc-lead"));
    drop(held);
    let led = leader.join().unwrap().expect("the leader's caller still gets the answer");
    let joined = joiner.join().unwrap().expect("the joiner gets the answer");
    assert!(!led.cancelled && !joined.cancelled);
    assert_eq!((text(&led), text(&joined)), ("answer".into(), "answer".into()));
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
    let held = rig.gate.acquire(Instant::now() + Duration::from_secs(5)).unwrap();
    let script = rig.script("joiner", "sleep 1.5; echo answer");
    let leader = rig.lead(Some("rc-l2"), script.clone());
    let joiner = rig.read(Some("rc-j2"), script);
    rig.wait_joined(1);
    let left = Instant::now();
    assert!(read_cancel::cancel("rc-j2"));
    let error = joiner.join().unwrap().expect_err("the joiner left");
    assert_eq!(error.message(), CANCELLED_MESSAGE);
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
    let held = rig.gate.acquire(Instant::now() + Duration::from_secs(5)).unwrap();
    let script = rig.script("late", "echo fresh");
    let first = rig.lead(Some("rc-late-1"), script.clone());
    let second = rig.read(Some("rc-late-2"), script.clone());
    rig.wait_joined(1);
    assert!(read_cancel::cancel("rc-late-1"));
    assert!(read_cancel::cancel("rc-late-2"));
    // The decision is made by the leader's poll while it waits in the queue.
    let until = Instant::now() + coverage_relaxed(Duration::from_secs(10));
    while super::shared_reads()
        .lock()
        .unwrap()
        .keys()
        .any(|key| key.cwd == rig.cwd)
    {
        assert!(Instant::now() < until, "the cancelled flight stayed joinable");
        thread::sleep(Duration::from_millis(5));
    }
    let late = rig.read(None, script);
    drop(held);
    let fresh = late.join().unwrap().expect("a fresh read");
    assert_eq!(text(&fresh), "fresh");
    assert!(!fresh.cancelled);
    assert!(first.join().unwrap().is_err(), "the cancelled leader got no answer");
    assert!(second.join().unwrap().is_err(), "the cancelled joiner got no answer");
    assert_eq!(rig.started().len(), 1, "only the fresh read started a child");
}

/// A read that never named an id is a waiter that never leaves.
#[cfg(unix)]
#[test]
fn a_waiter_without_an_id_keeps_the_read_alive() {
    let rig = Rig::new(1);
    let held = rig.gate.acquire(Instant::now() + Duration::from_secs(5)).unwrap();
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
        assert!(!read_cancel::cancel(&id), "a finished read is not registered");
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
    let arm = |cancellable: bool| {
        let rig = Rig::new(CLICKS + 2);
        let began = Instant::now();
        let mut readers = Vec::new();
        for click in 0..CLICKS {
            let id = format!("rc-burst-{cancellable}-{click}");
            readers.push(rig.read(
                cancellable.then_some(id.as_str()),
                rig.script(&format!("click{click}"), "exec sleep 20"),
            ));
            rig.wait_started(click + 1);
            if click > 0 && cancellable {
                read_cancel::cancel(&format!("rc-burst-{cancellable}-{}", click - 1));
            }
        }
        // The last click has settled: give the cancelled children their grace.
        thread::sleep(gone_within());
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
