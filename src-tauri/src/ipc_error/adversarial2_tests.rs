//! Adversarial round 2 against the thread-local typed-error record
//! (ft-5c1313c9). Tests marked `#[ignore = "break: ..."]` pin a defect found by
//! review: each asserts the behaviour a correct mechanism would have, and fails
//! today. Run them with `--ignored` to see the failure. The others pin
//! behaviour that holds and that a fix must keep.

use super::*;
use crate::engine::git_cli::{refusal_message, Refusal};
use crate::singleflight::SingleFlight;
use crate::test_support::coverage_relaxed;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

fn shed(label: &str) -> String {
    refusal_message(label, Refusal::Shed)
}

/// `cmd_get_commit_graph` runs its history walk in a `thread::scope` thread
/// (commands/commit_graph.rs) and returns that thread's error. The gate's
/// record is written on the scoped thread, so the command thread has none.
#[test]
#[ignore = "break: a deferral raised on a scoped helper thread is not on the command thread, so the read is never retried"]
fn a_deferral_raised_on_a_scoped_helper_thread_still_types_the_command() {
    let error = run_command(|| {
        std::thread::scope(|scope| {
            scope
                .spawn(|| Err::<(), _>(shed("git log")))
                .join()
                .expect("helper joins")
        })
    })
    .expect_err("the helper was refused");
    assert_eq!(error.kind, IpcErrorKind::DeferredUnderLoad, "{error}");
}

/// What a thread hop must never do: hand the command a kind another thread's
/// record wrote. The hop loses information (a missed retry), it does not add
/// any.
#[test]
fn a_helper_threads_record_never_types_the_command_that_spawned_it() {
    let error = run_command(|| {
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    typed_text(
                        IpcErrorKind::RepositoryTrustRequired,
                        "helper trust refusal".to_string(),
                    )
                })
                .join()
                .expect("helper joins");
        });
        Err::<(), _>("helper trust refusal".to_string())
    })
    .expect_err("failed");
    assert_eq!(error.kind, IpcErrorKind::Unclassified, "{error}");
}

fn run_flight(
    flights: &Arc<SingleFlight<&'static str, Result<(), String>>>,
    runs: &Arc<AtomicUsize>,
    leader_hold: Option<Duration>,
) -> Result<(), IpcError> {
    let flights = Arc::clone(flights);
    let runs = Arc::clone(runs);
    run_command(move || {
        flights.run("k", || {
            runs.fetch_add(1, Ordering::SeqCst);
            if let Some(hold) = leader_hold {
                std::thread::sleep(hold);
            }
            Err(shed("git blame"))
        })
    })
}

/// `ref_cache::through_stamped_coalesced` joins callers on one `SingleFlight`:
/// the leader's closure runs on the leader's thread and every waiter is handed
/// a clone of the leader's `Err` text. The record is on the leader's thread
/// only.
fn one_flight() -> (Result<(), IpcError>, Result<(), IpcError>, usize) {
    let flights = Arc::new(SingleFlight::<&'static str, Result<(), String>>::default());
    let runs = Arc::new(AtomicUsize::new(0));
    let leader = {
        let (flights, runs) = (flights.clone(), runs.clone());
        std::thread::spawn(move || {
            run_flight(
                &flights,
                &runs,
                Some(coverage_relaxed(Duration::from_millis(400))),
            )
        })
    };
    // The leader holds the flight for 400 ms; the waiter joins inside that.
    std::thread::sleep(coverage_relaxed(Duration::from_millis(100)));
    let waiter = run_flight(&flights, &runs, None);
    let leader = leader.join().expect("leader joins");
    (leader, waiter, runs.load(Ordering::SeqCst))
}

#[test]
fn a_single_flight_leader_is_typed_by_its_own_record() {
    let (leader, _waiter, runs) = one_flight();
    assert_eq!(runs, 1, "the waiter must have joined the leader's run");
    assert_eq!(
        leader.expect_err("refused").kind,
        IpcErrorKind::DeferredUnderLoad
    );
}

#[test]
#[ignore = "break: a single-flight waiter is handed the leader's refusal text but not the leader's record, so it is never retried"]
fn a_single_flight_waiter_given_the_leaders_refusal_is_typed_deferred() {
    let (_leader, waiter, runs) = one_flight();
    assert_eq!(runs, 1, "the waiter must have joined the leader's run");
    assert_eq!(
        waiter.expect_err("refused").kind,
        IpcErrorKind::DeferredUnderLoad
    );
}

/// A trust refusal written first and evicted by 16 later records is no longer
/// "written as trust", and because a deferral is on the list the loose rule
/// then types the same text `deferred_under_load`: the frontend retries a
/// trust refusal five times and never opens the trust prompt.
#[test]
#[ignore = "break: cap eviction turns a genuine trust refusal into a retryable deferral when a deferral is also on the list"]
fn an_evicted_trust_refusal_is_never_retyped_as_a_deferral() {
    let error = run_command(|| {
        let trust = typed_text(
            IpcErrorKind::RepositoryTrustRequired,
            "Repository trust required: Open /r in GitPulse".to_string(),
        );
        for n in 0..MAX_TYPED {
            let _ = shed(&format!("git probe {n}"));
        }
        Err::<(), _>(trust)
    })
    .expect_err("failed");
    assert_ne!(error.kind, IpcErrorKind::DeferredUnderLoad, "{error}");
}

/// Identical texts each take a slot, so a long run of one repeated refusal
/// pushes an earlier, different record off the list.
#[test]
#[ignore = "break: repeated identical records are not deduplicated and evict earlier distinct ones"]
fn repeating_one_record_does_not_evict_the_others() {
    let error = run_command(|| {
        let _declined = shed("git status");
        for _ in 0..(MAX_TYPED * 2) {
            let _ = typed_text(IpcErrorKind::StaleStash, "stale".to_string());
        }
        Err::<(), _>("fatal: bad object HEAD".to_string())
    })
    .expect_err("failed");
    assert_eq!(error.kind, IpcErrorKind::DeferredUnderLoad, "{error}");
}

/// The kind names no repository, and the frontend flags the tab of the repo
/// the command was called for. A command that reaches a second repository and
/// returns that one's refusal verbatim types the first repository's tab.
#[test]
fn trust_kind_is_attached_by_text_whichever_repository_the_text_names() {
    let other = "Repository trust required: Open /some/other/repo in GitPulse".to_string();
    let error = run_command(|| {
        let text = typed_text(IpcErrorKind::RepositoryTrustRequired, other.clone());
        Err::<(), _>(text)
    })
    .expect_err("failed");
    assert_eq!(error.kind, IpcErrorKind::RepositoryTrustRequired);
    assert!(
        error.detail.is_none(),
        "no repository is attached to the kind"
    );
}

/// The scope guard clears on unwind: a body that records and then panics
/// leaves nothing on the thread for the next command.
#[test]
fn a_panicking_body_leaves_the_thread_record_empty() {
    let panicked = std::panic::catch_unwind(|| {
        let _ = run_command(|| -> Result<(), String> {
            let _ = shed("git status");
            panic!("body panics after recording");
        });
    });
    assert!(panicked.is_err());
    assert!(!kind_was_written(IpcErrorKind::DeferredUnderLoad));
}
