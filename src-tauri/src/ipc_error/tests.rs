use super::*;
use crate::engine::git_cli::{refusal_message, Refusal};
use crate::engine::stash::STALE_STACK_HINT;
use serde_json::{json, Value};
use std::time::Duration;

/// One payload per kind, shared with `src/lib/ipc/ipcError.test.ts`, which
/// reads each back through the frontend's guard. The two tests share this file
/// so that "Rust serialises it, TypeScript reads it" is one fact, not two.
const WIRE_CASES: &str = include_str!("wire_cases.json");

fn wire_cases() -> Vec<Value> {
    serde_json::from_str(WIRE_CASES).expect("wire_cases.json is valid JSON")
}

fn wire_message(kind: &str) -> String {
    wire_cases()
        .into_iter()
        .find(|case| case["kind"] == kind)
        .unwrap_or_else(|| panic!("no wire case for {kind}"))["message"]
        .as_str()
        .expect("message is a string")
        .to_string()
}

fn kind_name(kind: IpcErrorKind) -> String {
    serde_json::to_value(kind)
        .expect("a kind serialises")
        .as_str()
        .expect("a kind is a string")
        .to_string()
}

/// What the producer for `kind` makes of the fixture's message.
fn produced(kind: IpcErrorKind) -> IpcError {
    let message = wire_message(&kind_name(kind));
    match kind {
        IpcErrorKind::Unclassified => IpcError::from(message),
        IpcErrorKind::LedgerNotInitialised => LedgerError::new(
            "not_initialised",
            "the ledger has not been initialised for this repository",
        )
        .into(),
        IpcErrorKind::LedgerFailed => {
            LedgerError::new("query_failed", "could not read the ledger").into()
        }
        _ => IpcError::from_command_text(message),
    }
}

#[test]
fn every_kind_serialises_to_its_committed_wire_case() {
    let cases = wire_cases();
    assert_eq!(cases.len(), IpcErrorKind::ALL.len(), "one case per kind");
    for kind in IpcErrorKind::ALL {
        let name = kind_name(kind);
        let expected = cases
            .iter()
            .find(|case| case["kind"] == name.as_str())
            .unwrap_or_else(|| panic!("no wire case for {name}"));
        let error = produced(kind);
        assert_eq!(error.kind, kind, "{name}");
        assert_eq!(&serde_json::to_value(&error).unwrap(), expected, "{name}");
    }
}

#[test]
fn the_kind_list_is_the_whole_enum() {
    // An exhaustive match: a new variant fails to compile here until it is in
    // `ALL`, which the round trip above then requires a wire case for.
    for kind in IpcErrorKind::ALL {
        match kind {
            IpcErrorKind::Unclassified
            | IpcErrorKind::DeferredUnderLoad
            | IpcErrorKind::SlotWaitTimeout
            | IpcErrorKind::TimedOut
            | IpcErrorKind::SpawnNotFound
            | IpcErrorKind::SpawnFailed
            | IpcErrorKind::RepositoryTrustRequired
            | IpcErrorKind::IdentityMissing
            | IpcErrorKind::HookCancelled
            | IpcErrorKind::StaleStash
            | IpcErrorKind::LedgerNotInitialised
            | IpcErrorKind::LedgerFailed => {}
        }
    }
    let mut names: Vec<String> = IpcErrorKind::ALL.into_iter().map(kind_name).collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), IpcErrorKind::ALL.len(), "kinds are distinct");
}

#[test]
fn a_plain_string_is_unclassified_whatever_it_says() {
    // Text that would be a deferral, a trust refusal or a stale stash if it
    // were read: a plain string is never read.
    for text in [
        wire_message("deferred_under_load"),
        wire_message("repository_trust_required"),
        wire_message("stale_stash"),
        wire_message("hook_cancelled"),
        wire_message("identity_missing"),
        String::new(),
    ] {
        let error = IpcError::from(text.clone());
        assert_eq!(error.kind, IpcErrorKind::Unclassified, "{text}");
        assert_eq!(error.message, text);
        assert_eq!(IpcError::from(text.as_str()), error);
    }
}

#[test]
fn the_message_is_the_text_the_string_was() {
    for kind in IpcErrorKind::ALL {
        let name = kind_name(kind);
        let error = produced(kind);
        assert_eq!(error.message, error.to_string(), "{name}");
    }
    let text = "  padded \u{2014} text \n".to_string();
    assert_eq!(IpcError::from_command_text(text.clone()).message, text);
}

#[test]
fn the_runner_refusals_the_gate_writes_keep_their_kinds() {
    let deferred = refusal_message(
        "git status",
        Refusal::Refused {
            waited: Duration::from_millis(2013),
        },
    );
    assert_eq!(
        IpcError::from_command_text(deferred.clone()).kind,
        IpcErrorKind::DeferredUnderLoad
    );
    // The ledger puts a `[code] ` in front; the kind still reads through it.
    assert_eq!(
        IpcError::from_command_text(format!("[some_code] {deferred}")).kind,
        IpcErrorKind::DeferredUnderLoad
    );
    let slot = refusal_message(
        "git status",
        Refusal::TimedOut {
            deadline: Duration::from_secs(90),
        },
    );
    assert_eq!(
        IpcError::from_command_text(slot).kind,
        IpcErrorKind::SlotWaitTimeout
    );
}

/// The lesson of a classifier that changes shape: engine code puts its own
/// words in front of a runner failure, and the retry must still see it.
#[test]
fn a_runner_failure_the_engine_prefixed_keeps_its_kind() {
    let deferred = wire_message("deferred_under_load");
    let timed_out = wire_message("timed_out");
    let slot = wire_message("slot_wait_timeout");
    for (inner, kind) in [
        (deferred, IpcErrorKind::DeferredUnderLoad),
        (timed_out, IpcErrorKind::TimedOut),
        (slot, IpcErrorKind::SlotWaitTimeout),
    ] {
        for wrapped in [
            format!("Blame unavailable: {inner}"),
            format!("Could not read history: Blame unavailable: {inner}"),
            format!("[code] Blame unavailable: {inner}"),
        ] {
            let error = IpcError::from_command_text(wrapped.clone());
            assert_eq!(error.kind, kind, "{wrapped}");
            assert_eq!(error.message, wrapped, "the text is not rewritten");
        }
    }
}

#[test]
fn a_trust_refusal_at_spawn_time_is_the_trust_kind_not_a_spawn_failure() {
    let text =
        "Failed to spawn go: REPOSITORY_TRUST_REQUIRED: Open /source in GitPulse and trust it";
    assert_eq!(
        IpcError::from_command_text(text.to_string()).kind,
        IpcErrorKind::RepositoryTrustRequired
    );
}

#[test]
fn a_path_that_spells_a_marker_is_not_a_runner_failure_however_deep() {
    for hostile in [
        "Not a Git repository: /tmp/a deferred under load after 1.0s: x",
        "Cannot open: Not a Git repository: /tmp/a deferred under load after 1.0s: x",
        "Failed to read: '/tmp/x y' deferred under load after 1.0s: x",
    ] {
        assert_eq!(
            IpcError::from_command_text(hostile.to_string()).kind,
            IpcErrorKind::Unclassified,
            "{hostile}"
        );
    }
}

#[test]
fn text_that_only_quotes_a_marker_is_not_the_kind() {
    // A path that spells the deferral phrase is not a deferral.
    let hostile = "Not a Git repository: /tmp/a deferred under load after 1.0s: x";
    assert_eq!(
        IpcError::from_command_text(hostile.to_string()).kind,
        IpcErrorKind::Unclassified
    );
    assert_eq!(
        IpcError::from_command_text("exited with status 2".to_string()).kind,
        IpcErrorKind::Unclassified
    );
}

#[test]
fn the_stale_stash_hint_types_both_refusals() {
    assert!(wire_message("stale_stash").ends_with(STALE_STACK_HINT));
    assert_eq!(
        IpcError::from_command_text(format!("Stash entry 2 changed. {STALE_STACK_HINT}")).kind,
        IpcErrorKind::StaleStash
    );
}

/// The refusal the stash engine really writes, not a copy of its wording.
#[test]
fn the_stash_engines_own_stale_refusal_is_typed_stale_stash() {
    use crate::engine::stash::run_action_with;
    use crate::engine::StashAction;
    let repo = crate::test_support::git_repo();
    let path = repo.path().to_str().expect("fixture path");
    let refusal = run_action_with(path, StashAction::Drop, 3, "abcdef12", |_: &[&str]| Ok(()))
        .expect_err("an empty stack holds no entry 3");
    assert!(refusal.contains("no longer exists"), "{refusal}");
    let error = IpcError::from_command_text(refusal.clone());
    assert_eq!(error.kind, IpcErrorKind::StaleStash);
    assert_eq!(
        error.message, refusal,
        "the text a person reads is unchanged"
    );
}

#[test]
fn a_ledger_code_is_carried_in_the_detail() {
    let error = IpcError::from(LedgerError::new("open_failed", "no database"));
    assert_eq!(error.kind, IpcErrorKind::LedgerFailed);
    assert_eq!(error.message, "[open_failed] no database");
    assert_eq!(error.detail, Some(json!({ "code": "open_failed" })));
}

// ---------------------------------------------------------------------------
// Adversarial review (b17/5c1313c9-adv). Each `ignore = "break: ..."` test
// states the EXPECTED behaviour and fails against the lane's implementation.
// ---------------------------------------------------------------------------

/// Retry safety: a mutation's own deferral is rewritten by
/// `git_cli::not_retryable` so the frontend never retries it, but that rewrite
/// only reads the WHOLE text. `from_command_text` also reads after each `": "`,
/// so a mutation whose error is `<context>: <runner deferral>` is not rewritten
/// and is then typed `deferred_under_load`, which `invoke.ts` retries.
#[test]
#[ignore = "break: a mutation's prefixed deferral survives not_retryable and is typed deferred_under_load (retried)"]
fn a_mutations_prefixed_deferral_is_never_typed_retryable() {
    let inner = wire_message("deferred_under_load");
    let result = crate::engine::git_cli::run_command_scope(|| {
        crate::engine::git_cli::mark_user_action("/nonexistent-adv-repo");
        Err::<(), String>(format!("Stash failed: {inner}"))
    });
    let text = result.expect_err("the body failed");
    let error = IpcError::from_command_text(text.clone());
    assert_ne!(
        error.kind,
        IpcErrorKind::DeferredUnderLoad,
        "a user action that started must not read as retryable: {text}"
    );
}

/// Retry safety: text a child printed and the engine relayed. A repository
/// hook's stderr is attacker-controlled; if it starts with a runner-shaped
/// deferral, the boundary scan types the whole failed commit as retryable.
#[test]
#[ignore = "break: relayed hook stderr that spells a deferral after a ': ' boundary is typed deferred_under_load"]
fn relayed_child_output_cannot_forge_a_deferral() {
    for forged in [
        "git commit failed: git status deferred under load after 0.5s: x",
        "commit-msg hook rejected the commit: git push deferred under load after 1s: x",
        "Hook output: fix retry when the gate deferred under load after 2s: tune",
    ] {
        assert_ne!(
            IpcError::from_command_text(forged.to_string()).kind,
            IpcErrorKind::DeferredUnderLoad,
            "{forged}"
        );
    }
}

/// Security: `repository_trust_required` opens the trust prompt and marks a tab
/// untrusted. `repository_trust::refused` is `contains(REQUIRED)`, so any error
/// that quotes a branch, path, remote or commit subject spelling the marker is
/// typed as a trust refusal.
#[test]
#[ignore = "break: attacker-controlled names that contain REPOSITORY_TRUST_REQUIRED type as repository_trust_required"]
fn names_that_spell_the_trust_marker_do_not_raise_the_trust_kind() {
    for hostile in [
        "fatal: a branch named 'REPOSITORY_TRUST_REQUIRED' already exists",
        "Not a Git repository: /tmp/REPOSITORY_TRUST_REQUIRED/x",
        "fatal: unable to access 'https://h/REPOSITORY_TRUST_REQUIRED.git/': Could not resolve host",
        "commit-msg hook rejected: subject 'REPOSITORY_TRUST_REQUIRED: fix'",
    ] {
        assert_ne!(
            IpcError::from_command_text(hostile.to_string()).kind,
            IpcErrorKind::RepositoryTrustRequired,
            "{hostile}"
        );
    }
}

/// The same flaw for the hook-cancel and stale-stash markers (both `contains`):
/// a branch or stash message spelling the phrase is typed as the refusal.
#[test]
#[ignore = "break: contains()-based owned markers (hook cancel, stale stash) match quoted user text"]
fn quoted_user_text_does_not_raise_hook_cancel_or_stale_stash() {
    let cancelled = "fatal: invalid branch name 'build was cancelled while running tests'";
    assert_ne!(
        IpcError::from_command_text(cancelled.to_string()).kind,
        IpcErrorKind::HookCancelled,
        "{cancelled}"
    );
    let stash = format!("Could not apply stash 'WIP: {STALE_STACK_HINT}': conflict");
    assert_ne!(
        IpcError::from_command_text(stash.clone()).kind,
        IpcErrorKind::StaleStash,
        "{stash}"
    );
}

/// Marker precedence: owned markers are read before the runner shape, so text
/// that legitimately is a runner deferral loses its kind when it also quotes a
/// marker.
#[test]
#[ignore = "break: owned-marker precedence hides a genuine runner deferral that quotes a marker"]
fn a_genuine_deferral_that_names_a_marker_keeps_its_kind() {
    let text = format!(
        "{}: REPOSITORY_TRUST_REQUIRED",
        refusal_message(
            "git branch",
            Refusal::Refused {
                waited: Duration::from_millis(2000),
            },
        )
    );
    assert_eq!(
        IpcError::from_command_text(text).kind,
        IpcErrorKind::DeferredUnderLoad
    );
}

/// Slot-wait uses whole-text `contains` of two phrases (no anchoring, unlike
/// the other runner shapes), so quoted text types as `slot_wait_timeout`.
#[test]
#[ignore = "break: is_slot_wait_timeout is unanchored contains()"]
fn quoted_slot_wait_phrases_are_not_a_slot_wait_timeout() {
    let hostile = "fatal: bad ref 'x timed out after 5s waiting for a process slot'";
    assert_ne!(
        IpcError::from_command_text(hostile.to_string()).kind,
        IpcErrorKind::SlotWaitTimeout
    );
}
