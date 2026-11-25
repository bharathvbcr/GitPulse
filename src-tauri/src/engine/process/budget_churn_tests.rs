//! The spawn budget's observability and churn tests (process side).

use super::gate::RefusalCause;
use super::{
    is_deferred_under_load, refusal_message, refusal_message_at, shared_budget, with_admission,
    with_background_processes, Admission, BucketLevel, Refusal, RefusingBucket, RunError,
    SpawnGate,
};
use crate::test_support::{coverage_relaxed, wait_until};
use std::time::{Duration, Instant};

fn level(bucket: RefusingBucket, tokens: u32, floor: u32, burst: u32) -> BucketLevel {
    BucketLevel {
        bucket,
        tokens,
        floor,
        burst,
    }
}

/// AC3. A refusal names the bucket that refused and the level it saw, and
/// keeps the sentence every classifier matches on.
#[test]
fn a_refusal_names_the_refusing_bucket_and_its_level() {
    // The shared record is drained; this process's own bucket is full.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spawn-budget.v2");
    let gate = SpawnGate::with_rate(8, 8, 1).sharing(Ok(path.clone()));
    let other = shared_budget::SharedBudget::open(&path).expect("second handle");
    while other.take(0, 8, 1) == shared_budget::Take::Granted {}
    let (refusal, judged) = with_admission(Admission::Reactive, || {
        gate.acquire_judged(Instant::now() + Duration::from_millis(200), &|| false)
    })
    .err()
    .expect("refused");
    assert!(matches!(refusal, Refusal::Refused { .. }), "{refusal:?}");
    assert_eq!(
        judged,
        Some(RefusalCause::Bucket(level(RefusingBucket::Shared, 0, 0, 8)))
    );
    let message = refusal_message_at("git status", refusal, judged);
    assert!(
        message.message().ends_with(
            "the git spawn rate limit admitted nothing sooner \
             (shared bucket: 0 token(s), needs more than 0, burst 8)"
        ),
        "{message}",
        message = message.message()
    );
    assert!(
        is_deferred_under_load(message.message()),
        "{message}",
        message = message.message()
    );
    assert_eq!(RunError::classify(message.message()), RunError::Deferred);

    // Background work, shed at once by this process's own bucket: the floor is
    // the reserve kept for reactive reads, and the level is below it.
    let local = SpawnGate::with_rate(8, 16, 1);
    local.state.lock().unwrap().tokens = 3;
    local.state.lock().unwrap().tokens_at = Some(Instant::now() + Duration::from_secs(86_400));
    let (refusal, judged) = with_background_processes(|| {
        local.acquire_judged(Instant::now() + Duration::from_secs(5), &|| false)
    })
    .err()
    .expect("shed");
    assert_eq!(refusal, Refusal::Shed);
    assert_eq!(
        judged,
        Some(RefusalCause::Bucket(level(RefusingBucket::Local, 3, 4, 16)))
    );
    let message = refusal_message_at("git worktree", refusal, judged);
    assert_eq!(
        message,
        "git worktree deferred under load after 0.000s: background work is shed while the git \
         spawn rate budget is low (local bucket: 3 token(s), needs more than 4, burst 16)"
    );
    assert!(
        is_deferred_under_load(message.message()),
        "{message}",
        message = message.message()
    );
    assert!(is_deferred_under_load(&format!(
        "[deferred_worktree] {message}",
        message = message.message()
    )));
    assert_eq!(RunError::classify(message.message()), RunError::Deferred);
}

/// The wording callers that cannot know the level construct is unchanged: the
/// text before the level is the text every test, classifier and TS reader
/// already matches.
#[test]
fn a_refusal_without_a_level_reads_exactly_as_before() {
    let waited = Duration::from_millis(2_013);
    assert_eq!(
        refusal_message("git status", Refusal::Refused { waited }),
        "git status deferred under load after 2.013s: the git spawn rate limit admitted nothing sooner"
    );
    assert_eq!(
        refusal_message("git status", Refusal::Shed),
        "git status deferred under load after 0.000s: background work is shed while the git \
         spawn rate budget is low"
    );
}

/// AC4, through the gate. Four threads keep asking a drained shared record for
/// a token for two seconds, the way four agents' hooks do. The record changes
/// only when a token is credited, so it is rewritten at most as often as the
/// refill rate grants one, not once per denied ask.
#[test]
fn four_denied_waiters_rewrite_the_shared_record_no_faster_than_it_refills() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spawn-budget.v2");
    let rate = 4u32;
    let gate: &'static SpawnGate = Box::leak(Box::new(
        SpawnGate::with_rate(16, 16, rate).sharing(Ok(path.clone())),
    ));
    let other = shared_budget::SharedBudget::open(&path).expect("second handle");
    while other.take(0, 16, rate) == shared_budget::Take::Granted {}
    let window = Duration::from_secs(2);
    let started = Instant::now();
    let end = started + window;
    let grants: u64 = std::thread::scope(|scope| {
        let waiters: Vec<_> = (0..4)
            .map(|_| {
                scope.spawn(move || {
                    let mut grants = 0u64;
                    while Instant::now() < end {
                        let got = with_admission(Admission::Reactive, || {
                            gate.acquire_judged(end, &|| false).is_ok()
                        });
                        grants += u64::from(got);
                    }
                    grants
                })
            })
            .collect();
        waiters.into_iter().map(|w| w.join().unwrap()).sum()
    });
    let secs = started.elapsed().as_secs_f64();
    let writes = gate.shared.as_ref().unwrap().writes();
    println!(
        "WAITER WRITES: 4 waiters x {secs:.2} s on a drained record at {rate}/s -> {writes} record writes \
         ({:.1}/s), {grants} grants",
        writes as f64 / secs
    );
    // One write per grant, and one probe per handle that proves the record
    // still takes writes; the slack covers a grant on the window's edges.
    let bound = (f64::from(rate) * secs).ceil() as u64 + 3;
    assert!(
        writes <= bound,
        "{writes} writes in {secs:.2} s at {rate}/s refill (bound {bound})"
    );
}

/// The wait for a token is bounded by the deadline and by cancellation even
/// though the waiter now sleeps until the token is due.
#[test]
fn a_waiter_asleep_for_a_token_still_ends_at_its_deadline_and_on_cancellation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spawn-budget.v2");
    let gate = SpawnGate::with_rate(8, 8, 1).sharing(Ok(path.clone()));
    let other = shared_budget::SharedBudget::open(&path).expect("second handle");
    while other.take(0, 8, 1) == shared_budget::Take::Granted {}
    let began = Instant::now();
    let refusal = with_admission(Admission::Reactive, || {
        gate.acquire_until(began + Duration::from_millis(150), &|| false)
    })
    .err();
    assert!(
        matches!(refusal, Some(Refusal::Refused { .. })),
        "{refusal:?}"
    );
    assert!(
        began.elapsed() < coverage_relaxed(Duration::from_secs(1)),
        "{:?}",
        began.elapsed()
    );

    let flag = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(Duration::from_millis(100));
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        let began = Instant::now();
        let refusal = with_admission(Admission::Reactive, || {
            gate.acquire_until(began + Duration::from_secs(30), &|| {
                flag.load(std::sync::atomic::Ordering::SeqCst)
            })
        })
        .err();
        assert_eq!(refusal, Some(Refusal::Cancelled));
        assert!(
            began.elapsed() < coverage_relaxed(Duration::from_secs(1)),
            "{:?}",
            began.elapsed()
        );
    });
}

/// A reactive read queued behind an older one while every slot is busy is
/// refused at the same 2 s as ever, but the refusal names the slots: it asked
/// the rate budget nothing, so the rate-limit sentence would name the wrong
/// cause. The front keeps its ticket and is admitted once a slot frees.
#[test]
fn a_reactive_read_behind_the_line_while_slots_are_full_names_the_slots() {
    let dir = tempfile::tempdir().unwrap();
    let gate: &'static SpawnGate = Box::leak(Box::new(
        SpawnGate::with_rate(1, 8, 1).sharing(Ok(dir.path().join("spawn-budget.v2"))),
    ));
    assert!(gate.shared.is_some());
    let held = with_admission(Admission::Reactive, || {
        gate.acquire(Instant::now() + Duration::from_secs(30))
    })
    .expect("the one slot");
    let front = std::thread::spawn(move || {
        with_admission(Admission::Reactive, || {
            gate.acquire_judged(Instant::now() + Duration::from_secs(10), &|| false)
        })
        .is_ok()
    });
    assert!(
        wait_until(Duration::from_secs(10), || {
            gate.state.lock().unwrap().reactive_line.len() == 1
        }),
        "the front never queued"
    );
    let began = Instant::now();
    let (refusal, cause) = with_admission(Admission::Reactive, || {
        gate.acquire_judged(Instant::now() + Duration::from_secs(10), &|| false)
    })
    .err()
    .expect("refused");
    let waited = began.elapsed();
    assert!(matches!(refusal, Refusal::Refused { .. }), "{refusal:?}");
    assert!(
        waited >= Duration::from_millis(1_900) && waited < coverage_relaxed(Duration::from_secs(3)),
        "the refusal timing moved: {waited:?}"
    );
    assert_eq!(
        cause,
        Some(RefusalCause::Line {
            in_flight: 1,
            limit: 1,
            ahead: 1
        })
    );
    let message = refusal_message_at("git status", refusal, cause);
    let text = message.message();
    assert!(is_deferred_under_load(text), "{text}");
    assert_eq!(RunError::classify(text), RunError::Deferred);
    assert!(text.contains("process slot"), "{text}");
    assert!(
        !text.contains("rate limit admitted nothing sooner"),
        "{text}"
    );
    drop(held);
    assert!(front.join().unwrap(), "the front kept its ticket");
}
