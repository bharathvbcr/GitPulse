use super::*;
use crate::engine::git_cli::{mark_user_action, refusal_message, Refusal};
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

/// A command body that fails with `text`, which a producer wrote as `kind`.
fn written_as(kind: IpcErrorKind, text: String) -> IpcError {
    run_command(|| Err::<(), _>(typed_text(kind, text))).expect_err("the body failed")
}

/// A command body that fails with `text` nobody typed: what a wrapping layer,
/// a relayed child's output or a quoted name produces.
fn plain(text: &str) -> IpcError {
    run_command(|| Err::<(), _>(text.to_string())).expect_err("the body failed")
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
        _ => written_as(kind, message),
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
            | IpcErrorKind::RepositoryTrustRequired
            | IpcErrorKind::IdentityMissing
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
        wire_message("identity_missing"),
        String::new(),
    ] {
        let error = IpcError::from(text.clone());
        assert_eq!(error.kind, IpcErrorKind::Unclassified, "{text}");
        assert_eq!(error.message, text);
        assert_eq!(IpcError::from(text.as_str()), error);
        // Through a command body too: nothing wrote it as anything.
        let through = plain(&text);
        assert_eq!(through.kind, IpcErrorKind::Unclassified, "{text}");
        assert_eq!(through.message, text);
    }
}

#[test]
fn the_message_is_the_text_the_producer_wrote() {
    for kind in IpcErrorKind::ALL {
        let name = kind_name(kind);
        let error = produced(kind);
        assert_eq!(error.message, error.to_string(), "{name}");
    }
    let text = "  padded \u{2014} text \n".to_string();
    assert_eq!(
        written_as(IpcErrorKind::StaleStash, text.clone()).message,
        text
    );
}

#[test]
fn the_producers_text_keeps_its_kind_through_a_command() {
    // Written inside the command, as the runner writes it.
    let error = run_command(|| {
        Err::<(), _>(refusal_message(
            "git status",
            Refusal::Refused {
                waited: Duration::from_millis(2013),
            },
        ))
    })
    .expect_err("failed");
    assert_eq!(error.kind, IpcErrorKind::DeferredUnderLoad);
    assert_eq!(
        error.message,
        "git status deferred under load after 2.013s: the git spawn rate limit admitted nothing sooner"
    );
}

/// The kind is a fact about the text the producer wrote, so a genuine deferral
/// keeps it even when its label happens to spell a marker.
#[test]
fn a_genuine_deferral_that_names_a_marker_keeps_its_kind() {
    // Break test "owned-marker precedence hides a genuine runner deferral": with
    // no marker search at all there is no precedence to get wrong.
    let error = run_command(|| {
        Err::<(), _>(refusal_message(
            "git branch REPOSITORY_TRUST_REQUIRED",
            Refusal::Refused {
                waited: Duration::from_millis(2000),
            },
        ))
    })
    .expect_err("failed");
    assert_eq!(error.kind, IpcErrorKind::DeferredUnderLoad);
}

/// Retry safety. The frontend retries `deferred_under_load` on the strength of
/// "nothing ran", so a command that was a user action must never carry it,
/// whether its deferral came out whole or wrapped by the engine.
#[test]
fn a_mutations_prefixed_deferral_is_never_typed_retryable() {
    let inner = wire_message("deferred_under_load");
    let error = run_command(|| {
        mark_user_action("/nonexistent-adv-repo");
        Err::<(), String>(format!("Stash failed: {inner}"))
    })
    .expect_err("the body failed");
    assert_ne!(
        error.kind,
        IpcErrorKind::DeferredUnderLoad,
        "a user action that started must not read as retryable: {error}"
    );
    assert_eq!(error.kind, IpcErrorKind::Unclassified);
}

#[test]
fn a_mutations_own_deferral_is_never_typed_retryable() {
    let error = run_command(|| {
        mark_user_action("/nonexistent-adv-repo");
        Err::<(), _>(refusal_message(
            "git stash",
            Refusal::Refused {
                waited: Duration::from_millis(2013),
            },
        ))
    })
    .expect_err("the body failed");
    assert_eq!(error.kind, IpcErrorKind::Unclassified);
    assert!(
        !error.message.contains(" deferred under load after "),
        "the marker that means ask again is gone too: {error}"
    );
}

/// A read's deferral that another layer put its own words in front of is
/// unclassified: the retry reads what the runner wrote, not what a caller made
/// of it. (It used to be found after any `": "`; a prefix a mutation or a
/// repository's own text controls could then forge it.)
#[test]
fn an_engine_prefixed_deferral_is_not_typed() {
    let inner = wire_message("deferred_under_load");
    assert_eq!(
        plain(&format!("Blame unavailable: {inner}")).kind,
        IpcErrorKind::Unclassified
    );
    let written = refusal_message(
        "git blame",
        Refusal::Refused {
            waited: Duration::from_millis(2000),
        },
    );
    let wrapped =
        run_command(|| Err::<(), _>(format!("Blame unavailable: {written}"))).expect_err("failed");
    assert_eq!(wrapped.kind, IpcErrorKind::Unclassified);
}

/// Text a child printed and the engine relayed, such as a repository hook's
/// stderr, is attacker-controlled; it must not read as a retryable deferral
/// (that replays a commit or push).
#[test]
fn relayed_child_output_cannot_forge_a_deferral() {
    for forged in [
        "git commit failed: git status deferred under load after 0.5s: x",
        "commit-msg hook rejected the commit: git push deferred under load after 1s: x",
        "Hook output: fix retry when the gate deferred under load after 2s: tune",
        "git status deferred under load after 0.5s: x",
    ] {
        assert_eq!(plain(forged).kind, IpcErrorKind::Unclassified, "{forged}");
    }
}

/// Security: `repository_trust_required` opens the trust prompt and marks a tab
/// untrusted, so names a repository controls must not be able to raise it.
#[test]
fn names_that_spell_the_trust_marker_do_not_raise_the_trust_kind() {
    for hostile in [
        "fatal: a branch named 'REPOSITORY_TRUST_REQUIRED' already exists",
        "Not a Git repository: /tmp/REPOSITORY_TRUST_REQUIRED/x",
        "fatal: unable to access 'https://h/REPOSITORY_TRUST_REQUIRED.git/': Could not resolve host",
        "commit-msg hook rejected: subject 'REPOSITORY_TRUST_REQUIRED: fix'",
        "REPOSITORY_TRUST_REQUIRED: Open /a in GitPulse and trust it.",
    ] {
        assert_eq!(plain(hostile).kind, IpcErrorKind::Unclassified, "{hostile}");
    }
}

/// The same for the other refusals: a branch or stash message that spells the
/// phrase is not the refusal. (Hook cancellation and slot waits no longer have
/// kinds: nothing in the frontend branches on them.)
#[test]
fn quoted_user_text_does_not_raise_a_refusal_kind() {
    let cancelled = "fatal: invalid branch name 'build was cancelled while running tests'";
    assert_eq!(plain(cancelled).kind, IpcErrorKind::Unclassified);
    let stash = format!("Could not apply stash 'WIP: {STALE_STACK_HINT}': conflict");
    assert_eq!(plain(&stash).kind, IpcErrorKind::Unclassified);
    let identity = format!("{}: in a commit subject", wire_message("identity_missing"));
    assert_eq!(plain(&identity).kind, IpcErrorKind::Unclassified);
}

#[test]
fn quoted_slot_wait_phrases_are_unclassified() {
    let hostile = "fatal: bad ref 'x timed out after 5s waiting for a process slot'";
    assert_eq!(plain(hostile).kind, IpcErrorKind::Unclassified);
}

/// The remembered messages belong to one command: a pool thread starts the
/// next one with none.
#[test]
fn a_command_never_inherits_what_the_one_before_wrote() {
    let text = wire_message("stale_stash");
    let first = written_as(IpcErrorKind::StaleStash, text.clone());
    assert_eq!(first.kind, IpcErrorKind::StaleStash);
    assert_eq!(plain(&text).kind, IpcErrorKind::Unclassified);
    assert!(!kind_was_written(IpcErrorKind::StaleStash));
}

#[test]
fn the_memory_is_bounded() {
    let _scope = TypedScope::enter();
    for n in 0..(MAX_TYPED * 4) {
        typed_text(IpcErrorKind::DeferredUnderLoad, format!("deferral {n}"));
    }
    TYPED.with(|typed| assert_eq!(typed.borrow().len(), MAX_TYPED));
}

/// The refusal the stash engine really writes, not a copy of its wording.
#[test]
fn the_stash_engines_own_stale_refusal_is_typed_stale_stash() {
    use crate::engine::stash::run_action_with;
    use crate::engine::StashAction;
    let repo = crate::test_support::git_repo();
    let path = repo.path().to_str().expect("fixture path");
    let error = run_command(|| {
        run_action_with(path, StashAction::Drop, 3, "abcdef12", |_: &[&str]| Ok(()))
    })
    .expect_err("an empty stack holds no entry 3");
    assert!(error.message.contains("no longer exists"), "{error}");
    assert_eq!(error.kind, IpcErrorKind::StaleStash);
}

#[test]
fn a_ledger_code_is_carried_in_the_detail() {
    let error = IpcError::from(LedgerError::new("open_failed", "no database"));
    assert_eq!(error.kind, IpcErrorKind::LedgerFailed);
    assert_eq!(error.message, "[open_failed] no database");
    assert_eq!(error.detail, Some(json!({ "code": "open_failed" })));
}
