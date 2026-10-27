//! Adversarial checks on lane L5's drop count (ft-878db72b), written by a
//! reviewer who did not write it. A failing test here is a claim that is false.
//!
//! The count's contract, from `notify_drops.rs` and `wiki/MCP and Agents.md`:
//! one increment for a report whose reply missed its bound, and none for any
//! other outcome.

use super::notify_drops::NotifyDrops;
use super::*;

fn session_input() -> HookInput {
    HookInput {
        hook_event_name: "SessionEnd".into(),
        session_id: "claude-abc".into(),
        cwd: "/Users/me/GitPulse".into(),
        ..HookInput::default()
    }
}

/// `within_budget` answers `None` for a deadline, but also for a worker that
/// died before it could answer. A send that panics did not "miss its bound":
/// the reply did not arrive late, nothing was awaited. Counting it says the
/// report was abandoned at its deadline, and the stderr line says the same.
#[test]
fn adv_l5_a_send_that_panics_is_not_counted_as_a_missed_reply() {
    let serial = crate::test_support::env::env_serial();
    let _env = crate::test_support::env::bind_env(&serial)
        .set(crate::alerts::bridge::SESSION_ENV, "term-4-1")
        .set(crate::alerts::bridge::SOCKET_ENV, "/tmp/gitpulse-test.sock");
    let drops = NotifyDrops::new();
    let error = run_notify_within(
        Some("session_ended"),
        &session_input(),
        |_, _| panic!("worker fault, not a late reply"),
        &drops,
        // Far longer than the worker takes to die: a deadline cannot be what
        // ended the wait.
        Some(Duration::from_secs(30)),
    )
    .expect_err("a worker that died did not deliver");
    assert_eq!(
        drops.snapshot().dropped,
        0,
        "a panicking send was counted as a reply that missed its bound"
    );
    assert!(
        !error.to_string().contains("exceeded its deadline"),
        "a panic was reported as a deadline miss: {error}"
    );
}

/// `record` advertises `saturating_add`, but it saturates the *returned* value
/// only: `fetch_add` has already wrapped the stored one, so the next snapshot
/// reads 0 and a count that reached `u64::MAX` reports that nothing was dropped.
/// Unreachable in a one-shot hook, so this is the claim in the code, not a
/// production fault. The count is built at `u64::MAX` through the test
/// constructor, as its field is private to its module (and no longer the only
/// one, so it can no longer be built by layout).
#[test]
fn adv_l5_the_drop_count_saturates_instead_of_wrapping_to_zero() {
    let drops = NotifyDrops::starting_at(u64::MAX);
    assert_eq!(drops.snapshot().dropped, u64::MAX);
    drops.record();
    assert_eq!(
        drops.snapshot().dropped,
        u64::MAX,
        "the count wrapped past u64::MAX back to a smaller number"
    );
}

/// For every event but `session_ended` the hook's bound
/// ([`notify_input::budgets_for`]) is `NOTIFY_BUDGET`, 1.5 s, which is also
/// what `exchange` sets as the socket's own read timeout. A host that accepts
/// and never answers is therefore abandoned by whichever of two equal timers
/// fires first. When the socket's wins, `exchange` returns `Err("no answer from
/// GitPulse: ...")`, which is reported as an ordinary failure and not counted:
/// a reply that missed its bound, uncounted. (`session_ended` is safe: 300 ms
/// against 1500 ms.) Each trial is the production path with the production
/// budget; a fix that counts both ends of the give-up passes every one.
#[cfg(unix)]
#[test]
fn adv_l5_a_silent_host_is_counted_for_every_event_not_only_session_end() {
    use std::os::unix::net::UnixListener;
    let serial = crate::test_support::env::env_serial();
    let dir = tempfile::tempdir_in("/tmp").expect("a short socket directory");
    let path = dir.path().join("h.sock");
    let listener = UnixListener::bind(&path).expect("bind");
    // Accepts and holds every connection open without reading or replying.
    std::thread::spawn(move || {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept() {
            held.push(stream);
        }
    });
    let _env = crate::test_support::env::bind_env(&serial)
        .set(crate::alerts::bridge::SESSION_ENV, "term-4-1")
        .set(
            crate::alerts::bridge::SOCKET_ENV,
            path.to_str().expect("utf-8 path"),
        );
    const TRIALS: u64 = 5;
    let drops = NotifyDrops::new();
    let mut uncounted = Vec::new();
    for trial in 0..TRIALS {
        let before = drops.snapshot().dropped;
        let error = run_notify_within(
            Some("tool_finished"),
            &session_input(),
            notify_send,
            &drops,
            None,
        )
        .expect_err("a host that never answers did not deliver");
        if drops.snapshot().dropped != before + 1 {
            uncounted.push(format!("trial {trial}: {error}"));
        }
    }
    assert!(
        uncounted.is_empty(),
        "{} of {TRIALS} abandoned reports were not counted: {uncounted:#?}",
        uncounted.len()
    );
}
