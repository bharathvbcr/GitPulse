//! Adversarial round 2 against the typed command error (ft-5c1313c9, then
//! ft-b595921a). Each test pins a defect found by review of the thread-local
//! record this replaced, restated for the error that now carries its own kind:
//! a refusal raised on a helper thread, a flood of other refusals, a detail
//! that names the repository.

use super::*;
use crate::engine::git_cli::{refusal_message, Refusal};
use crate::ipc_error::CommandError;
use crate::procguard::LockedSpawn;

fn shed(label: &str) -> CommandError {
    refusal_message(label, Refusal::Shed)
}

/// A read that is on the replay-safe list.
fn read<T>(body: impl FnOnce() -> Result<T, CommandError>) -> Result<T, IpcError> {
    run_command_as(Some("cmd_get_status"), body)
}

/// `cmd_get_commit_graph` runs its history walk in a `thread::scope` thread
/// (commands/commit_graph.rs) and returns that thread's error. The refusal
/// crosses the join inside the error, so the read is typed deferred, and
/// retried, as it is when the walk runs inline, with nothing handed back
/// beside it.
#[test]
fn a_deferral_raised_on_a_scoped_helper_thread_types_an_allowlisted_read() {
    let error = read(|| {
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

/// The join adds nothing a command may not already have: a command that is
/// not on the replay-safe list, and a user action, still never carry the
/// retryable kind out of a helper's refusal.
#[test]
fn a_helper_deferral_never_makes_a_write_or_unlisted_command_retryable() {
    let carried = || {
        std::thread::scope(|scope| {
            scope
                .spawn(|| Err::<(), _>(shed("git log")))
                .join()
                .expect("helper joins")
        })
    };
    let wrapped =
        || carried().map_err(|refused| refused.wrap(|refused| format!("Stash failed: {refused}")));
    let inline =
        |command| run_command_as(command, || Err::<(), _>(shed("git log"))).expect_err("refused");
    for command in [Some("cmd_commit"), None] {
        // Only the loose rule (any wording) is reserved for the allowlist; an
        // exact refusal is typed the same whether it came from a helper or not.
        let rewritten = run_command_as(command, wrapped).expect_err("refused");
        assert_eq!(rewritten.kind, IpcErrorKind::Unclassified, "{rewritten}");
        let exact = run_command_as(command, carried).expect_err("refused");
        assert_eq!(exact.kind, inline(command).kind, "{exact}");
    }
    let user_action = read(|| {
        crate::engine::git_cli::mark_user_action("/nonexistent-adv-repo");
        carried()
    })
    .expect_err("refused");
    assert_eq!(
        user_action.kind,
        IpcErrorKind::Unclassified,
        "{user_action}"
    );
}

/// What a thread hop must never do: hand the command a kind it did not return.
/// A helper's typed refusal does not type a plain error that merely spells the
/// same text.
#[test]
fn a_helper_threads_refusal_never_types_a_plain_copy_of_its_text() {
    let error = run_command(|| {
        let refusal = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    CommandError::typed(
                        IpcErrorKind::RepositoryTrustRequired,
                        "helper trust refusal",
                    )
                })
                .join()
                .expect("helper joins")
        });
        Err::<(), _>(CommandError::plain(refusal.into_message()))
    })
    .expect_err("failed");
    assert_eq!(error.kind, IpcErrorKind::Unclassified, "{error}");
}

/// A trust refusal must survive any number of refusals the command swallowed
/// before it: the prompt it opens is the only way out of the refusal.
#[test]
fn a_trust_refusal_survives_a_flood_of_deferrals() {
    for floods in [16, 20, 200] {
        let error = read(|| {
            for n in 0..floods {
                let _ = shed(&format!("git probe {n}"));
            }
            Err::<(), _>(CommandError::typed(
                IpcErrorKind::RepositoryTrustRequired,
                "Repository trust required: Open /r in GitPulse",
            ))
        })
        .expect_err("failed");
        assert_eq!(
            error.kind,
            IpcErrorKind::RepositoryTrustRequired,
            "{floods} deferrals: {error}"
        );
    }
}

#[test]
fn identity_and_stale_stash_survive_a_flood_of_deferrals_too() {
    for (kind, text) in [
        (
            IpcErrorKind::IdentityMissing,
            "Git does not know who you are: x",
        ),
        (
            IpcErrorKind::StaleStash,
            "Stash entry 1 changed. Refresh the stash list.",
        ),
    ] {
        let error = read(|| {
            for n in 0..40 {
                let _ = shed(&format!("git probe {n}"));
            }
            Err::<(), _>(CommandError::typed(kind, text))
        })
        .expect_err("failed");
        assert_eq!(error.kind, kind, "{error}");
    }
}

/// Deferrals the read swallowed say nothing about the failure it returns: the
/// loose rule types an error whose own origin is the gate's refusal, never an
/// unrelated one after a refusal the read recovered from (owner decision,
/// 2026-10-26: the thread-local record typed these, the error value does not).
#[test]
fn swallowed_deferrals_do_not_type_an_unrelated_failure() {
    for swallowed in [1, 17, 1000] {
        let error = read(|| {
            for n in 0..swallowed {
                let _ = shed(&format!("git probe {n}"));
            }
            Err::<(), _>(CommandError::plain("fatal: bad object HEAD"))
        })
        .expect_err("failed");
        assert_eq!(error.kind, IpcErrorKind::Unclassified, "{error}");
        assert_eq!(error.message, "fatal: bad object HEAD");
    }
}

/// A trust refusal names the repository it is about, so the frontend can tell a
/// refusal for the tab it is acting on from one a command met on the way.
#[test]
fn the_trust_kind_carries_the_repository_the_refusal_is_about() {
    let other = "Repository trust required: Open /some/other/repo in GitPulse".to_string();
    let error = run_command(|| {
        Err::<(), _>(CommandError::typed_with_detail(
            IpcErrorKind::RepositoryTrustRequired,
            other.clone(),
            serde_json::json!({ "path": "/some/other/repo", "common_dir": "/some/other/repo/.git" }),
        ))
    })
    .expect_err("failed");
    assert_eq!(error.kind, IpcErrorKind::RepositoryTrustRequired);
    assert_eq!(
        error.detail,
        Some(
            serde_json::json!({ "path": "/some/other/repo", "common_dir": "/some/other/repo/.git" })
        )
    );
    let wire = serde_json::to_value(&error).expect("serialises");
    assert_eq!(wire["detail"]["path"], "/some/other/repo");
}

/// A wrapped trust refusal keeps no detail: the words are another layer's, so
/// the error is unclassified and says nothing about which repository.
#[test]
fn a_wrapped_trust_refusal_carries_no_kind_and_no_detail() {
    let error = run_command(|| {
        Err::<(), _>(
            CommandError::typed_with_detail(
                IpcErrorKind::RepositoryTrustRequired,
                "Repository trust required: Open /r in GitPulse",
                serde_json::json!({ "path": "/r" }),
            )
            .wrap(|refusal| format!("Could not list worktrees: {refusal}")),
        )
    })
    .expect_err("failed");
    assert_eq!(error.kind, IpcErrorKind::Unclassified);
    assert_eq!(error.detail, None);
}

/// The real producer attaches it: the refusal for a repository nobody approved
/// names that repository's path.
#[test]
fn the_real_trust_refusal_names_its_repository_in_the_detail() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let status = std::process::Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(dir.path())
        .status_locked()
        .expect("git init");
    assert!(status.success());
    let canonical = dir
        .path()
        .canonicalize()
        .expect("canonical")
        .display()
        .to_string();
    let error = run_command(|| crate::repository_trust::require(dir.path()))
        .expect_err("nobody approved this repository");
    assert_eq!(error.kind, IpcErrorKind::RepositoryTrustRequired, "{error}");
    let path = error
        .detail
        .as_ref()
        .and_then(|detail| detail["path"].as_str())
        .expect("the detail names the repository");
    assert_eq!(path, canonical);
}

/// A refusal raised while a non-git program was being spawned reaches the
/// caller inside the spawn failure's own words, with its origin kept.
#[test]
fn a_trust_refusal_inside_a_spawn_failure_keeps_its_origin() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let status = std::process::Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(dir.path())
        .status_locked()
        .expect("git init");
    assert!(status.success());
    let error = crate::engine::git_cli::run_command_in(
        "true",
        &[],
        std::time::Duration::from_secs(5),
        Some(dir.path()),
    )
    .expect_err("nobody approved this repository");
    assert!(error.message().starts_with("Failed to spawn"), "{error:?}");
    assert_eq!(
        error.origin(),
        Some(IpcErrorKind::RepositoryTrustRequired),
        "{error:?}"
    );
}
