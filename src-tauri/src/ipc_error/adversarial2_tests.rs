//! Adversarial round 2 against the thread-local typed-error record
//! (ft-5c1313c9). Each test pins a defect found by review as the behaviour a
//! correct mechanism has. The ones for the cap, the dedupe, the detail and the
//! single-flight waiter were fixed; the helper-thread one is a characterization
//! that pins a known, fail-safe gap for the typed-threading follow-up card.

use super::*;
use crate::engine::git_cli::{refusal_message, Refusal};
use crate::procguard::LockedSpawn;

fn shed(label: &str) -> String {
    refusal_message(label, Refusal::Shed)
}

/// A read that is on the replay-safe list.
fn read<T>(body: impl FnOnce() -> Result<T, String>) -> Result<T, IpcError> {
    run_command_as(Some("cmd_get_status"), body)
}

/// CHARACTERIZATION of a known gap, not a defect to fix in this card.
///
/// `cmd_get_commit_graph` runs its history walk in a `thread::scope` thread
/// (commands/commit_graph.rs) and returns that thread's error. The gate writes
/// its record on the helper thread, so the command thread has none and the
/// deferral is unclassified: the read is shown the refusal instead of retried.
/// That fails safe (it loses a retry, it never adds one). Carrying a helper's
/// records back to its command is part of the typed-threading follow-up card;
/// when it lands, flip this test to expect `DeferredUnderLoad`.
#[test]
fn a_deferral_raised_on_a_scoped_helper_thread_is_unclassified_until_helper_records_are_carried() {
    let error = read(|| {
        std::thread::scope(|scope| {
            scope
                .spawn(|| Err::<(), _>(shed("git log")))
                .join()
                .expect("helper joins")
        })
    })
    .expect_err("the helper was refused");
    assert_eq!(error.kind, IpcErrorKind::Unclassified, "{error}");
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

/// A trust refusal written first must survive any number of later records: the
/// prompt it opens is the only way out of the refusal. Singletons are never
/// evicted, and a deferral overflow switches the loose rule off rather than
/// letting it retype the refusal.
#[test]
fn a_trust_refusal_survives_a_flood_of_deferrals() {
    for floods in [MAX_DEFERRALS, MAX_DEFERRALS + 4, 20, 200] {
        let error = read(|| {
            let trust = typed_text(
                IpcErrorKind::RepositoryTrustRequired,
                "Repository trust required: Open /r in GitPulse".to_string(),
            );
            for n in 0..floods {
                let _ = shed(&format!("git probe {n}"));
            }
            Err::<(), _>(trust)
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
            let written = typed_text(kind, text.to_string());
            for n in 0..40 {
                let _ = shed(&format!("git probe {n}"));
            }
            Err::<(), _>(written)
        })
        .expect_err("failed");
        assert_eq!(error.kind, kind, "{error}");
    }
}

/// Once more distinct deferrals were written than the cap holds, the record is
/// incomplete, and the loose rule fails closed: the read is not retried on a
/// guess.
#[test]
fn a_deferral_overflow_switches_the_loose_rule_off() {
    let error = read(|| {
        for n in 0..(MAX_DEFERRALS + 1) {
            let _ = shed(&format!("git probe {n}"));
        }
        Err::<(), _>("fatal: bad object HEAD".to_string())
    })
    .expect_err("failed");
    assert_eq!(error.kind, IpcErrorKind::Unclassified, "{error}");
}

/// Repeating one record does not use up slots, so it cannot push others off:
/// the same refusal written a thousand times is one entry.
#[test]
fn repeating_one_record_does_not_evict_the_others() {
    let error = read(|| {
        let _declined = shed("git status");
        for _ in 0..(MAX_DEFERRALS * 40) {
            let _ = typed_text(IpcErrorKind::StaleStash, "stale".to_string());
            let _ = shed("git status");
        }
        Err::<(), _>("fatal: bad object HEAD".to_string())
    })
    .expect_err("failed");
    assert_eq!(error.kind, IpcErrorKind::DeferredUnderLoad, "{error}");
    TYPED.with(|typed| assert!(typed.borrow().deferrals.len() <= 1));
}

/// A trust refusal names the repository it is about, so the frontend can tell a
/// refusal for the tab it is acting on from one a command met on the way.
#[test]
fn the_trust_kind_carries_the_repository_the_refusal_is_about() {
    let other = "Repository trust required: Open /some/other/repo in GitPulse".to_string();
    let error = run_command(|| {
        let text = typed_text_with_detail(
            IpcErrorKind::RepositoryTrustRequired,
            other.clone(),
            serde_json::json!({ "path": "/some/other/repo", "common_dir": "/some/other/repo/.git" }),
        );
        Err::<(), _>(text)
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
