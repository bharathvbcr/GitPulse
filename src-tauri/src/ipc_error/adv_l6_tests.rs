//! Adversarial review of lane L6 (the commit graph's helper thread), restated
//! for the typed command error: the refusal crosses the join in the error
//! itself, so there is no hand-back to forget, leak or duplicate.

use super::*;
use crate::engine::git_cli::{refusal_message, Refusal};
use crate::ipc_error::CommandError;

fn shed(label: &str) -> CommandError {
    refusal_message(label, Refusal::Shed)
}

fn read<T>(body: impl FnOnce() -> Result<T, CommandError>) -> Result<T, IpcError> {
    run_command_as(Some("cmd_get_commit_graph"), body)
}

/// The commit-graph shape: the helper walks, the command joins it and returns
/// its error.
fn helper_deferral() -> Result<(), CommandError> {
    std::thread::scope(|scope| {
        scope
            .spawn(|| Err::<(), _>(shed("git log")))
            .join()
            .expect("helper joins")
    })
}

/// Commands back to back on ONE pooled thread: a refusal the first one met
/// never types the next.
#[test]
fn adv_l6_a_pool_thread_carries_no_kind_into_the_next_command() {
    std::thread::spawn(|| {
        let ok = read(|| {
            let _ = helper_deferral();
            Ok::<_, CommandError>(1)
        });
        assert_eq!(ok.expect("ok"), 1);
        let failed = read(helper_deferral).expect_err("deferred");
        assert_eq!(failed.kind, IpcErrorKind::DeferredUnderLoad);
        let next = read(|| Err::<(), _>(CommandError::plain("plain failure"))).expect_err("fails");
        assert_eq!(next.kind, IpcErrorKind::Unclassified, "{next}");
        let again = read(|| Err::<(), _>(shed("git log"))).expect_err("fails");
        assert_eq!(
            again.kind,
            IpcErrorKind::DeferredUnderLoad,
            "this command returned the refusal itself, so it is typed: {again}"
        );
        let clean =
            read(|| Err::<(), _>(CommandError::plain("git log failed"))).expect_err("fails");
        assert_eq!(clean.kind, IpcErrorKind::Unclassified, "{clean}");
    })
    .join()
    .expect("thread");
}

/// A helper that is refused and then panics hands nothing back: the join is
/// Err, and the command's own error is not deferred.
#[test]
fn adv_l6_a_helper_that_panics_after_a_refusal_types_nothing() {
    let error = read(|| {
        let joined = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let _ = shed("git log");
                    panic!("walker died");
                })
                .join()
        });
        assert!(joined.is_err());
        Err::<(), _>(CommandError::plain("history walker panicked"))
    })
    .expect_err("fails");
    assert_eq!(error.kind, IpcErrorKind::Unclassified, "{error}");
}

/// A helper's swallowed deferral, then the command's own trust or identity
/// refusal: the returned error's own kind is the answer.
#[test]
fn adv_l6_a_command_threads_trust_or_identity_refusal_beats_a_helpers_deferral() {
    let trust = read(|| {
        let _ = helper_deferral();
        Err::<(), _>(CommandError::typed(
            IpcErrorKind::RepositoryTrustRequired,
            "Repository trust required: Open /r",
        ))
    })
    .expect_err("fails");
    assert_eq!(trust.kind, IpcErrorKind::RepositoryTrustRequired, "{trust}");
    let identity = read(|| {
        let _ = helper_deferral();
        Err::<(), _>(CommandError::typed(
            IpcErrorKind::IdentityMissing,
            "Git does not know who you are",
        ))
    })
    .expect_err("fails");
    assert_eq!(identity.kind, IpcErrorKind::IdentityMissing, "{identity}");
}

/// A helper's deferral the command swallowed, then an UNRELATED untyped error:
/// unclassified on every command, allowlisted or not. The record this replaced
/// typed it retryable on the allowlist; the owner narrowed that rule on
/// 2026-10-26 (ft-b595921a), since the error that comes out is the one the
/// command is about.
#[test]
fn adv_l6_an_unrelated_error_after_a_helper_deferral_is_never_retryable() {
    let body = || {
        let _ = helper_deferral();
        Err::<(), _>(CommandError::plain("unknown revision"))
    };
    for command in [
        Some("cmd_get_commit_graph"),
        Some("cmd_commit"),
        Some("cmd_stash_apply"),
        None,
    ] {
        let error = run_command_as(command, body).expect_err("fails");
        assert_eq!(
            error.kind,
            IpcErrorKind::Unclassified,
            "{command:?}: {error}"
        );
    }
}

/// A helper deferred and the command still answered: an Ok result stays Ok.
#[test]
fn adv_l6_a_helpers_refusal_on_an_ok_command_is_harmless() {
    let ok = read(|| {
        let _ = helper_deferral();
        Ok::<_, CommandError>("partial")
    })
    .expect("ok stays ok");
    assert_eq!(ok, "partial");
}

/// Two hops (a helper inside a helper, the way a single-flight leader runs
/// inside a command's helper) lose nothing: the refusal is in the value.
#[test]
fn adv_l6_a_refusal_crosses_two_thread_hops_with_its_kind() {
    let error = read(|| {
        std::thread::scope(|outer| {
            outer
                .spawn(|| {
                    std::thread::scope(|inner| {
                        inner
                            .spawn(|| Err::<(), _>(shed("leader git")))
                            .join()
                            .expect("inner joins")
                    })
                    .map_err(|refused| refused.wrap(|r| format!("Blame unavailable: {r}")))
                })
                .join()
                .expect("outer joins")
        })
    })
    .expect_err("fails");
    assert_eq!(error.kind, IpcErrorKind::DeferredUnderLoad, "{error}");
    assert!(error.message.starts_with("Blame unavailable: "), "{error}");
}

/// The allowlist holds no mutating command name.
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
