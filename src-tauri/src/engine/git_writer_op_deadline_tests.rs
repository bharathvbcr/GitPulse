//! The asker's deadline on an op snapshot: the app refuses to capture state
//! once the asker has stopped waiting (no ref, no snapshotted row), keeps as a
//! normal snapshot any state it captured in time, and ties its ledger rows to
//! the request id the hook's own row carries.
//!
//! No test here asserts on elapsed time. The deadline is judged by a clock the
//! test scripts call by call, so "expired before the lock", "expired after the
//! lock" and "expired only after the ref was written" are states the test
//! chooses, not races it hopes to win.

use super::*;
use crate::test_support::{git_in, git_repo, trust_repo};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const BUDGET: Duration = Duration::from_secs(2);

/// One answer of the scripted clock.
struct Step {
    at: Instant,
    /// Set when this call is made.
    mark: Option<&'static AtomicBool>,
    /// Waited for, bounded, before this call answers.
    wait_for: Option<&'static AtomicBool>,
}

thread_local! {
    static SCRIPT: RefCell<VecDeque<Step>> = const { RefCell::new(VecDeque::new()) };
}

/// Answers the scripted instants in order, and the last one for every call
/// after the script runs out. Per thread, so tests that run side by side do not
/// see each other's.
fn scripted_now() -> Instant {
    let step = SCRIPT.with(|script| {
        let mut script = script.borrow_mut();
        if script.len() > 1 {
            script.pop_front()
        } else {
            script.front().map(|last| Step {
                at: last.at,
                mark: None,
                wait_for: None,
            })
        }
    });
    let step = step.expect("the scripted clock was read with no script");
    if let Some(flag) = step.mark {
        flag.store(true, Ordering::Release);
    }
    if let Some(flag) = step.wait_for {
        wait_until(flag);
    }
    step.at
}

/// Spins, yielding, until `flag` is set. The bound only turns a hung test into
/// a failure; no assertion depends on how long it took.
fn wait_until(flag: &AtomicBool) {
    let give_up = Instant::now() + Duration::from_secs(120);
    while !flag.load(Ordering::Acquire) {
        assert!(Instant::now() < give_up, "the other side never arrived");
        std::thread::yield_now();
    }
}

fn script(steps: Vec<Step>) {
    SCRIPT.with(|script| *script.borrow_mut() = steps.into());
}

fn at(instant: Instant) -> Step {
    Step {
        at: instant,
        mark: None,
        wait_for: None,
    }
}

/// A deadline expiring at `base + BUDGET`, judged by the scripted clock.
fn deadline(base: Instant) -> AskerDeadline {
    AskerDeadline::with_clock(base + BUDGET, scripted_now)
}

fn inside(base: Instant) -> Instant {
    base + BUDGET - Duration::from_millis(1)
}

fn outside(base: Instant) -> Instant {
    base + BUDGET + Duration::from_millis(1)
}

fn repo() -> (tempfile::TempDir, String) {
    let dir = git_repo();
    git_in(dir.path(), &["config", "user.name", "Ada"]);
    git_in(dir.path(), &["config", "user.email", "ada@gitpulse.dev"]);
    git_in(dir.path(), &["commit", "--allow-empty", "-q", "-m", "one"]);
    trust_repo(dir.path());
    let canon = dir.path().canonicalize().unwrap();
    let path = canon.to_string_lossy().into_owned();
    (dir, path)
}

fn ask<'a>(id: &'a str, deadline: AskerDeadline) -> OpAsk<'a> {
    OpAsk {
        request_id: Some(id),
        deadline: Some(deadline),
    }
}

fn record(repo: &str, ask: &OpAsk<'_>) -> Result<OpSnapshotSummary, CommandError> {
    GitWriter::record_op_snapshot_with_limits_asked(
        repo,
        "git reset --hard HEAD",
        Some("sess-1"),
        None,
        crate::ledger::ids::now_millis(),
        MAX_OPS,
        OP_EXPIRY_SECS,
        ask,
    )
}

fn rows(repo: &str) -> Vec<crate::ledger::LedgerEvent> {
    crate::ledger::query_readonly_page(
        repo,
        &crate::ledger::EventQuery {
            descending: true,
            limit: 20,
            ..Default::default()
        },
        64 * 1024,
    )
    .expect("query ledger")
    .map(|page| page.events)
    .unwrap_or_default()
}

fn detail(row: &crate::ledger::LedgerEvent) -> serde_json::Value {
    serde_json::from_str(row.detail_json.as_deref().unwrap_or("{}")).expect("detail is json")
}

fn snapshot_rows(repo: &str) -> Vec<crate::ledger::LedgerEvent> {
    rows(repo)
        .into_iter()
        .filter(|row| {
            row.object
                .as_deref()
                .is_some_and(|object| object.starts_with(OP_REF_PREFIX))
        })
        .collect()
}

fn refs(repo: &str) -> usize {
    GitWriter::list_op_snapshots(repo, 50)
        .expect("list")
        .entries
        .len()
}

#[test]
fn a_deadline_that_passed_before_the_lock_refuses_with_no_ref_and_no_snapshotted_row() {
    let (_dir, repo) = repo();
    let base = Instant::now();
    script(vec![at(outside(base))]);

    let error = record(&repo, &ask("op-1", deadline(base))).expect_err("late work is refused");

    assert!(
        error.message().contains("deadline"),
        "the failure must say the deadline passed: {}",
        error.message()
    );
    assert_eq!(
        refs(&repo),
        0,
        "no snapshot ref for a snapshot nobody awaits"
    );
    assert!(snapshot_rows(&repo).is_empty(), "no snapshotted row either");
}

#[test]
fn the_lock_held_past_the_deadline_leaves_no_snapshot_when_it_is_released() {
    static PAST_THE_FIRST_CHECK: AtomicBool = AtomicBool::new(false);
    let (_dir, repo) = repo();
    let base = Instant::now();
    let canon = validate_repo(&repo).unwrap();
    let lock = repo_mutation_lock(&canon);
    let held = lock.lock().unwrap();

    let asked = {
        let repo = repo.clone();
        std::thread::spawn(move || {
            // In time when it asks, past the deadline by the time it has the lock.
            script(vec![
                Step {
                    at: inside(base),
                    mark: Some(&PAST_THE_FIRST_CHECK),
                    wait_for: None,
                },
                at(outside(base)),
            ]);
            record(&repo, &ask("op-2", deadline(base)))
        })
    };
    // It passed its first check, so it is at or about to take the held lock.
    wait_until(&PAST_THE_FIRST_CHECK);
    drop(held);

    let error = asked
        .join()
        .expect("the asking thread")
        .expect_err("the lock came too late");
    assert!(error.message().contains("deadline"), "{}", error.message());
    assert_eq!(refs(&repo), 0);
    assert!(snapshot_rows(&repo).is_empty());
}

#[test]
fn an_answer_exactly_at_the_deadline_has_missed_it() {
    let (_dir, repo) = repo();
    let base = Instant::now();
    script(vec![at(base + BUDGET)]);

    record(&repo, &ask("op-3", deadline(base))).expect_err("the boundary instant is past it");
    assert_eq!(refs(&repo), 0);

    script(vec![at(base + BUDGET - Duration::from_nanos(1))]);
    let summary = record(&repo, &ask("op-3", deadline(base))).expect("one tick inside is in time");
    assert_eq!(summary.seq, 1);
}

#[test]
fn a_deadline_that_passed_after_the_state_was_read_writes_no_ref_and_no_snapshotted_row() {
    let (_dir, repo) = repo();
    let base = Instant::now();
    // In time before the lock and after it; past by the moment the ref would be
    // written. The state was read, but the asker's write may already have run.
    script(vec![at(inside(base)), at(inside(base)), at(outside(base))]);

    let error = record(&repo, &ask("op-4", deadline(base))).expect_err("captured too late");

    assert!(error.message().contains("deadline"), "{}", error.message());
    assert_eq!(
        refs(&repo),
        0,
        "no ref for state captured past the deadline"
    );
    assert!(snapshot_rows(&repo).is_empty());
}

#[test]
fn state_captured_in_time_is_a_normal_snapshot_however_late_the_write_finishes() {
    let (_dir, repo) = repo();
    let base = Instant::now();
    // Every check the rule makes is in time; anything read after them would be
    // past the deadline, and must change nothing.
    script(vec![
        at(inside(base)),
        at(inside(base)),
        at(inside(base)),
        at(outside(base)),
    ]);

    let summary = record(&repo, &ask("op-4b", deadline(base))).expect("captured in time");

    assert_eq!(
        SCRIPT.with(|script| script.borrow().len()),
        1,
        "the deadline is read exactly three times: before the lock, after it, before the ref"
    );
    assert_eq!(refs(&repo), 1, "the ref is kept");
    let rows = snapshot_rows(&repo);
    assert_eq!(rows.len(), 1);
    let detail = detail(&rows[0]);
    assert_eq!(rows[0].object.as_deref(), Some(summary.op_ref.as_str()));
    assert_eq!(detail["snapshot"], "snapshotted", "{detail}");
    assert_eq!(detail["request_id"], "op-4b");
    assert!(
        detail
            .as_object()
            .unwrap()
            .keys()
            .all(|key| !key.contains("late")),
        "no late-marked refs exist: {detail}"
    );
}

#[test]
fn a_snapshot_inside_the_deadline_is_clean_and_carries_the_request_id() {
    let (_dir, repo) = repo();
    let base = Instant::now();
    script(vec![at(inside(base))]);

    record(&repo, &ask("op-5", deadline(base))).expect("in time");

    let rows = snapshot_rows(&repo);
    assert_eq!(rows.len(), 1);
    let detail = detail(&rows[0]);
    assert_eq!(detail["snapshot"], "snapshotted");
    assert_eq!(detail["request_id"], "op-5");
}

#[test]
fn a_request_nobody_set_a_deadline_for_is_never_marked_late() {
    let (_dir, repo) = repo();

    GitWriter::record_op_snapshot_with_limits(
        &repo,
        "git reset --hard HEAD",
        None,
        None,
        crate::ledger::ids::now_millis(),
        MAX_OPS,
        OP_EXPIRY_SECS,
    )
    .expect("recorded");

    let rows = snapshot_rows(&repo);
    let detail = detail(&rows[0]);
    assert_eq!(detail["snapshot"], "snapshotted");
    assert!(detail.get("request_id").is_none(), "{detail}");
}

#[test]
fn a_second_request_that_waited_out_the_first_is_refused_while_the_first_stands() {
    static FIRST_HOLDS_THE_LOCK: AtomicBool = AtomicBool::new(false);
    static SECOND_IS_QUEUED: AtomicBool = AtomicBool::new(false);
    let (_dir, repo) = repo();
    let base = Instant::now();

    let first = {
        let repo = repo.clone();
        std::thread::spawn(move || {
            // Its own checks are in time; it stops at the check after the lock
            // until the second request has been through its first check.
            script(vec![
                at(inside(base)),
                Step {
                    at: inside(base),
                    mark: Some(&FIRST_HOLDS_THE_LOCK),
                    wait_for: Some(&SECOND_IS_QUEUED),
                },
                at(inside(base)),
            ]);
            record(&repo, &ask("op-first", deadline(base)))
        })
    };
    wait_until(&FIRST_HOLDS_THE_LOCK);
    let second = {
        let repo = repo.clone();
        std::thread::spawn(move || {
            script(vec![
                Step {
                    at: inside(base),
                    mark: Some(&SECOND_IS_QUEUED),
                    wait_for: None,
                },
                at(outside(base)),
            ]);
            record(&repo, &ask("op-second", deadline(base)))
        })
    };

    let first = first.join().expect("first").expect("the first was in time");
    let second = second
        .join()
        .expect("second")
        .expect_err("the second waited too long");

    assert_eq!(first.seq, 1);
    assert!(
        second.message().contains("deadline"),
        "{}",
        second.message()
    );
    assert_eq!(refs(&repo), 1, "exactly the first request's snapshot");
    let rows = snapshot_rows(&repo);
    assert_eq!(rows.len(), 1);
    assert_eq!(detail(&rows[0])["request_id"], "op-first");
}

fn request(id: &str, cwd: &str) -> crate::desktop::instance::Request {
    crate::desktop::instance::Request::RecordGitOp {
        id: id.to_string(),
        cwd: cwd.to_string(),
        command: "git reset --hard HEAD".to_string(),
        session_id: None,
        task_id: None,
    }
}

#[test]
fn serving_a_request_received_too_long_ago_answers_the_deadline_and_logs_it_under_the_id() {
    let (_dir, repo) = repo();
    let received = Instant::now()
        .checked_sub(crate::desktop::instance::OP_RECORD_DEADLINE + Duration::from_secs(1))
        .expect("the host has been up longer than the op-snapshot budget");

    let answer = GitWriter::serve_op_record(&request("op-6", &repo), received).expect("served");

    assert_eq!(answer["snapshotted"], false, "{answer}");
    assert!(
        answer["error"]
            .as_str()
            .unwrap_or_default()
            .contains("deadline"),
        "{answer}"
    );
    assert_eq!(refs(&repo), 0);
    assert!(snapshot_rows(&repo).is_empty());
    let unsnapshotted: Vec<_> = rows(&repo)
        .into_iter()
        .filter(|row| row.object.as_deref() == Some("unsnapshotted"))
        .collect();
    assert_eq!(unsnapshotted.len(), 1, "the refusal is logged, loudly");
    assert_eq!(detail(&unsnapshotted[0])["request_id"], "op-6");
}

#[test]
fn serving_a_request_received_just_now_snapshots_it_under_the_id() {
    let (_dir, repo) = repo();

    let answer =
        GitWriter::serve_op_record(&request("op-7", &repo), Instant::now()).expect("served");

    assert_eq!(answer["snapshotted"], true, "{answer}");
    let rows = snapshot_rows(&repo);
    assert_eq!(rows.len(), 1);
    assert_eq!(detail(&rows[0])["request_id"], "op-7");
}
