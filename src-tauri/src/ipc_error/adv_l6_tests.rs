//! Adversarial review of lane L6 (`with_records` + the helper hand-back).

use super::*;
use crate::engine::git_cli::{refusal_message, Refusal};

fn shed(label: &str) -> String {
    refusal_message(label, Refusal::Shed)
}

fn read<T>(body: impl FnOnce() -> Result<T, String>) -> Result<T, IpcError> {
    run_command_as(Some("cmd_get_commit_graph"), body)
}

/// The commit-graph shape: helper walks, command absorbs at the join.
fn helper_deferral() -> Result<(), String> {
    let (result, records) = std::thread::scope(|scope| {
        scope
            .spawn(|| with_records(|| Err::<(), _>(shed("git log"))))
            .join()
            .expect("helper joins")
    });
    absorb(records);
    result
}

fn deferral_count() -> usize {
    TYPED.with(|typed| typed.borrow().deferrals.len())
}

/// Attack 1a/1c: two commands back to back on ONE pooled thread. The first
/// absorbs a helper deferral and returns Ok, then Err; the next must start
/// clean and never be retyped retryable.
#[test]
fn adv_l6_absorbed_records_do_not_leak_into_the_next_command_on_the_same_thread() {
    std::thread::spawn(|| {
        let ok = read(|| {
            let _ = helper_deferral();
            Ok::<_, String>(1)
        });
        assert_eq!(ok.expect("ok"), 1);
        assert_eq!(deferral_count(), 0, "scope exit resets");
        let failed = read(helper_deferral).expect_err("deferred");
        assert_eq!(failed.kind, IpcErrorKind::DeferredUnderLoad);
        assert_eq!(deferral_count(), 0, "scope exit resets after Err too");
        let next = read(|| Err::<(), _>("plain failure".to_string())).expect_err("fails");
        assert_eq!(next.kind, IpcErrorKind::Unclassified, "{next}");
        let again = read(|| Err::<(), _>(shed("git log"))).expect_err("fails");
        assert_eq!(
            again.kind,
            IpcErrorKind::DeferredUnderLoad,
            "this command wrote the text itself, so it is typed: {again}"
        );
        let clean = read(|| Err::<(), _>("git log failed".to_string())).expect_err("fails");
        assert_eq!(clean.kind, IpcErrorKind::Unclassified, "{clean}");
    })
    .join()
    .expect("thread");
}

/// Attack 1b + 5: a helper writes a record and then panics. No record
/// survives, the join is Err, and the command's error is not deferred.
#[test]
fn adv_l6_a_helper_that_panics_after_writing_leaves_no_record() {
    let error = read(|| {
        let joined = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    with_records(|| {
                        let _ = shed("git log");
                        panic!("walker died");
                    })
                })
                .join()
        });
        assert!(joined.is_err());
        assert_eq!(deferral_count(), 0);
        Err::<(), _>("history walker panicked".to_string())
    })
    .expect_err("fails");
    assert_eq!(error.kind, IpcErrorKind::Unclassified, "{error}");
}

/// A helper is a fresh thread: it never carries the command thread's records.
#[test]
fn adv_l6_a_scoped_helper_starts_with_empty_records() {
    let _ = run_command(|| {
        let _ = shed("command thread deferral");
        let (_, records) =
            std::thread::scope(|scope| scope.spawn(|| with_records(|| ())).join().expect("joins"));
        assert!(records.deferrals.is_empty());
        Ok::<_, String>(())
    });
}

/// Attack 2: helper deferral absorbed, command thread returns its own trust
/// refusal / identity refusal. The returned error's own kind wins.
#[test]
fn adv_l6_a_command_threads_trust_or_identity_refusal_beats_a_helpers_deferral() {
    let trust = read(|| {
        let _ = helper_deferral();
        Err::<(), _>(typed_text(
            IpcErrorKind::RepositoryTrustRequired,
            "Repository trust required: Open /r".to_string(),
        ))
    })
    .expect_err("fails");
    assert_eq!(trust.kind, IpcErrorKind::RepositoryTrustRequired, "{trust}");
    let identity = read(|| {
        let _ = helper_deferral();
        Err::<(), _>(typed_text(
            IpcErrorKind::IdentityMissing,
            "Git does not know who you are".to_string(),
        ))
    })
    .expect_err("fails");
    assert_eq!(identity.kind, IpcErrorKind::IdentityMissing, "{identity}");
}

/// Attack 2 (loose rule): helper deferral absorbed, but the command returns an
/// UNRELATED untyped error. On an allowlisted read this is typed retryable (the
/// documented loose rule); on any other command it is not.
#[test]
fn adv_l6_an_unrelated_error_after_a_helper_deferral_is_retryable_only_on_the_allowlist() {
    let body = || {
        let _ = helper_deferral();
        Err::<(), _>("unknown revision".to_string())
    };
    let listed = read(body).expect_err("fails");
    assert_eq!(listed.kind, IpcErrorKind::DeferredUnderLoad, "{listed}");
    for command in [Some("cmd_commit"), Some("cmd_stash_apply"), None] {
        let other = run_command_as(command, body).expect_err("fails");
        assert_eq!(
            other.kind,
            IpcErrorKind::Unclassified,
            "{command:?}: {other}"
        );
    }
}

/// Attack 3: helper deferred, command still Ok. Nothing is attached to an Ok
/// result and the record dies with the scope.
#[test]
fn adv_l6_a_stale_helper_record_on_an_ok_command_is_harmless() {
    let ok = read(|| {
        let _ = helper_deferral();
        Ok::<_, String>("partial")
    })
    .expect("ok stays ok");
    assert_eq!(ok, "partial");
    assert!(!kind_was_written(IpcErrorKind::DeferredUnderLoad));
}

/// Attack 4: nesting on one thread and across a hop. Nothing lost, nothing
/// duplicated (deferrals are a deduplicated set).
#[test]
fn adv_l6_nested_with_records_neither_loses_nor_duplicates() {
    let _ = run_command(|| {
        let ((inner_value, inner), outer) = with_records(|| {
            let _ = shed("outer probe");
            with_records(|| {
                let _ = shed("inner probe");
                7
            })
        });
        assert_eq!(inner_value, 7);
        assert_eq!(inner.deferrals.len(), 2, "inner snapshot holds both");
        assert_eq!(outer.deferrals.len(), 2);
        absorb(outer);
        absorb(inner);
        assert_eq!(deferral_count(), 2);
        Ok::<_, String>(())
    });
    let error = read(|| {
        let (result, records) = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    with_records(|| {
                        let (answer, leader) = with_records(|| Err::<(), _>(shed("leader git")));
                        absorb(leader);
                        answer
                    })
                })
                .join()
                .expect("joins")
        });
        absorb(records);
        assert_eq!(deferral_count(), 1);
        result
    })
    .expect_err("fails");
    assert_eq!(error.kind, IpcErrorKind::DeferredUnderLoad, "{error}");
}

/// Attack 4b: with_records snapshots the WHOLE thread record, not just the
/// work's. A record the leader's command wrote before the flight would travel
/// to every waiter. This test asserts the clean behaviour; a failure is the
/// finding.
#[test]
#[ignore = "FINDING: with_records snapshots the whole thread record, not just its work"]
fn adv_l6_with_records_returns_only_what_its_work_wrote() {
    let _ = run_command(|| {
        let _ = shed("written before the flight");
        let (_, records) = with_records(|| ());
        assert!(
            records.deferrals.is_empty(),
            "with_records leaks pre-existing thread records: {:?}",
            records.deferrals
        );
        Ok::<_, String>(())
    });
}

/// Attack 6: the allowlist holds no mutating command name.
#[test]
fn adv_l6_replay_safe_reads_hold_no_mutating_name() {
    for name in REPLAY_SAFE_READS {
        for verb in [
            "commit_changes",
            "push",
            "pull",
            "checkout",
            "merge",
            "rebase",
            "delete",
            "create",
            "apply",
            "pop",
            "drop",
            "stage",
            "discard",
            "write",
            "cmd_set_",
            "revert",
            "cherry",
        ] {
            assert!(!name.contains(verb), "{name} looks mutating ({verb})");
        }
    }
}
