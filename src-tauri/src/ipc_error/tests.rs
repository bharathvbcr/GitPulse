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

#[test]
fn a_ledger_code_is_carried_in_the_detail() {
    let error = IpcError::from(LedgerError::new("open_failed", "no database"));
    assert_eq!(error.kind, IpcErrorKind::LedgerFailed);
    assert_eq!(error.message, "[open_failed] no database");
    assert_eq!(error.detail, Some(json!({ "code": "open_failed" })));
}
