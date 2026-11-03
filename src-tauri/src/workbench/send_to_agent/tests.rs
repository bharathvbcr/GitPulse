use super::*;
use crate::engine::git_cli::git_global;
use std::sync::Arc;

const SESSION: &str = "6f1c2a7e-0d4b-4c1e-9a55-3b0e8f2d9c11";

fn run(kind: &str, provider: &str, state: &str, id: &str) -> Value {
    json!({"id": id, "kind": kind, "provider": provider, "state": state, "cwd": "/checkout"})
}

fn never_listening(_: &str) -> bool {
    false
}

fn no_conversation() -> Value {
    panic!("the conversation was looked for")
}

fn no_manvi() -> Result<Value, WorkbenchError> {
    panic!("Manvi was asked")
}

fn found() -> Value {
    json!({"ok": true, "resumable": true, "session_id": SESSION, "cwd": "/checkout", "permission_mode": "inspect"})
}

#[test]
fn a_live_claude_terminal_attempt_is_reached_through_its_mailbox() {
    for state in ["starting", "running"] {
        let route = decide(
            &run("external_terminal", "claude", state, SESSION),
            never_listening,
            no_conversation,
            no_manvi,
        );
        assert_eq!(
            route,
            Route::Mailbox {
                session: SESSION.into(),
                listening: false
            },
            "{state}"
        );
    }
    let route = decide(
        &run("external_terminal", "claude", "running", SESSION),
        |s| s == SESSION,
        no_conversation,
        no_manvi,
    );
    assert_eq!(
        route,
        Route::Mailbox {
            session: SESSION.into(),
            listening: true
        }
    );
    // The mailbox is named by the lower-case id whatever case the run has.
    let upper = SESSION.to_ascii_uppercase();
    let route = decide(
        &run("external_terminal", "claude", "running", &upper),
        never_listening,
        no_conversation,
        no_manvi,
    );
    assert_eq!(
        route,
        Route::Mailbox {
            session: SESSION.into(),
            listening: false
        }
    );
}

#[test]
fn a_live_attempt_with_no_session_id_or_not_claude_has_nobody_to_wake() {
    for (provider, id, why) in [
        ("claude", "run-1", "no Claude Code session id"),
        ("codex", SESSION, "not Claude Code"),
        ("grok", SESSION, "not Claude Code"),
    ] {
        let route = decide(
            &run("external_terminal", provider, "running", id),
            never_listening,
            no_conversation,
            no_manvi,
        );
        let Route::Unavailable { reason } = route else {
            panic!("{provider}/{id} was given a route");
        };
        assert!(reason.contains(why), "{reason}");
    }
}

#[test]
fn an_ended_claude_attempt_resumes_only_with_evidence_of_a_saved_conversation() {
    for (state, kind) in [
        ("exited", "external_terminal"),
        ("failed", "external_terminal"),
        ("cancelled", "external_terminal"),
        ("exited", "managed"),
    ] {
        let route = decide(
            &run(kind, "claude", state, SESSION),
            never_listening,
            found,
            no_manvi,
        );
        assert_eq!(route, Route::Resume { found: found() }, "{kind}/{state}");
    }
    let nothing = json!({"ok": true, "resumable": false, "reason": "Claude Code has no saved conversation for this attempt."});
    let route = decide(
        &run("external_terminal", "claude", "exited", SESSION),
        never_listening,
        || nothing.clone(),
        no_manvi,
    );
    assert_eq!(
        route,
        Route::Unavailable {
            reason: "Claude Code has no saved conversation for this attempt.".into()
        }
    );
    // A search that could not run is passed on as what it is.
    let unknown = json!({"ok": true, "resumable": false, "reason": "Could not check for a saved conversation: denied"});
    let Route::Unavailable { reason } = decide(
        &run("external_terminal", "claude", "exited", SESSION),
        never_listening,
        || unknown.clone(),
        no_manvi,
    ) else {
        panic!("an unchecked conversation was resumed");
    };
    assert!(reason.starts_with("Could not check"), "{reason}");
    // Only Claude Code conversations can be resumed.
    let Route::Unavailable { reason } = decide(
        &run("managed", "codex", "exited", SESSION),
        never_listening,
        no_conversation,
        no_manvi,
    ) else {
        panic!("a codex run was resumed");
    };
    assert!(
        reason.contains("only a Claude Code conversation"),
        "{reason}"
    );
}

/// A conversation resumed from an attempt keeps the attempt's session id, so a
/// hook waiting on that id means the conversation is live now and the message
/// belongs there, not in a second resume that would be refused as a duplicate.
#[test]
fn an_ended_attempt_whose_resumed_conversation_is_listening_is_reached_through_it() {
    let route = decide(
        &run("external_terminal", "claude", "exited", SESSION),
        |s| s == SESSION,
        no_conversation,
        no_manvi,
    );
    assert_eq!(
        route,
        Route::Mailbox {
            session: SESSION.into(),
            listening: true
        }
    );
}

#[test]
fn a_live_managed_run_is_reached_by_what_manvi_says_it_can_take() {
    let answer = |state: &str| Ok(json!({"ok": true, "state": state, "turn": 1, "max_turns": 3}));
    let route = decide(
        &run("managed", "claude", "running", "run-1"),
        never_listening,
        no_conversation,
        || answer("parked"),
    );
    assert_eq!(
        route,
        Route::Managed {
            state: "parked".into(),
            turn: 1,
            max_turns: 3
        }
    );
    let route = decide(
        &run("managed", "codex", "starting", "run-1"),
        never_listening,
        no_conversation,
        || answer("running"),
    );
    assert_eq!(
        route,
        Route::Managed {
            state: "running".into(),
            turn: 1,
            max_turns: 3
        }
    );
    for state in ["ended", "unresolved"] {
        let Route::Unavailable { reason } = decide(
            &run("managed", "claude", "running", "run-1"),
            never_listening,
            no_conversation,
            || answer(state),
        ) else {
            panic!("{state} was offered a follow-up");
        };
        assert!(reason.contains(state), "{reason}");
    }
}

#[test]
fn a_manvi_that_cannot_take_follow_ups_is_said_to_be_that_and_a_failed_ask_is_said_to_be_unchecked()
{
    let old = managed_run::follow_up_answer(Err(WorkbenchError::new(
        "unsupported_operation",
        "not advertised",
    )));
    let Route::Unavailable { reason } = decide(
        &run("managed", "claude", "running", "run-1"),
        never_listening,
        no_conversation,
        || old,
    ) else {
        panic!("an old Manvi was offered a follow-up");
    };
    assert!(
        reason.contains("This Manvi version cannot take follow-ups"),
        "{reason}"
    );

    let unprepared = managed_run::follow_up_answer(Err(WorkbenchError::new(
        "E_CONTINUE_UNSUPPORTED",
        "this run was prepared without max_continuations",
    )));
    let Route::Unavailable { reason } = decide(
        &run("managed", "claude", "running", "run-1"),
        never_listening,
        no_conversation,
        || unprepared,
    ) else {
        panic!("an unprepared run was offered a follow-up");
    };
    assert!(
        reason.contains("prepared without max_continuations"),
        "{reason}"
    );

    let down = Err(WorkbenchError::new(
        "unavailable",
        "the profile worker is shutting down",
    ));
    let Route::Unavailable { reason } = decide(
        &run("managed", "claude", "running", "run-1"),
        never_listening,
        no_conversation,
        || down,
    ) else {
        panic!("a failed ask was read as an answer");
    };
    assert!(reason.contains("nothing was checked"), "{reason}");
}

#[test]
fn an_attempt_that_has_not_started_or_whose_outcome_is_unknown_is_not_sent_anything() {
    for (state, why) in [
        ("prepared", "has not started"),
        ("unresolved", "unresolved"),
    ] {
        let Route::Unavailable { reason } = decide(
            &run("external_terminal", "claude", state, SESSION),
            |_| true,
            no_conversation,
            no_manvi,
        ) else {
            panic!("{state} was given a route");
        };
        assert!(reason.contains(why), "{reason}");
    }
    let Route::Unavailable { .. } = decide(&json!({}), |_| true, no_conversation, no_manvi) else {
        panic!("an empty run was given a route");
    };
}

#[test]
fn only_the_asked_for_check_runs_for_each_route() {
    // A live terminal attempt never looks for a transcript or asks Manvi; an
    // ended one never asks Manvi; a managed one never looks for a transcript.
    decide(
        &run("external_terminal", "claude", "running", SESSION),
        never_listening,
        no_conversation,
        no_manvi,
    );
    decide(
        &run("external_terminal", "claude", "exited", SESSION),
        never_listening,
        found,
        no_manvi,
    );
    decide(
        &run("managed", "claude", "running", "r"),
        never_listening,
        no_conversation,
        || Ok(json!({"ok": true, "state": "parked", "turn": 1, "max_turns": 2})),
    );
}

fn host(dir: &std::path::Path) -> (WorkbenchState, std::path::PathBuf) {
    let root = dir.join("repo");
    git_global(&["init", root.to_str().unwrap()]).unwrap();
    crate::test_support::trust_repo(&root);
    let state = WorkbenchState(Arc::new(super::super::Inner {
        path: Some(dir.join("profile.sqlite")),
        ..Default::default()
    }));
    state
        .register(root.to_str().unwrap(), "repo", "register")
        .unwrap();
    state.request("items.put", &json!({"id":"task","request_id":"task","expected_revision":0,"title":"Fix CI","repository_ids":["repo"],"primary_repository_id":"repo"}).to_string()).unwrap();
    (state, root)
}

fn prepare(state: &WorkbenchState, root: &std::path::Path, method: &str, id: &str, provider: &str) {
    state.request(method, &json!({"id":id,"request_id":format!("prepare-{id}"),"task_id":"task","source_revision":1,"repository_id":"repo","repository_revision":1,"provider":provider,"permission_mode":"inspect","repo_path":root}).to_string()).unwrap();
}

fn claim(state: &WorkbenchState, id: &str, kind: Option<&str>) {
    let mut input = json!({"id":id,"request_id":format!("claim-{id}"),"expected_revision":1,"owner_id":"owner","session_id":format!("session-{id}")});
    if let Some(kind) = kind {
        input["kind"] = json!(kind);
    }
    state
        .with_store(|store| query(store, "runs.claim", &input.to_string()))
        .unwrap();
}

fn ask(id: &str, text: &str, request: &str, confirm: bool, expect: Option<&str>) -> String {
    let mut input = json!({"id": id, "text": text, "request_id": request, "confirm": confirm});
    if let Some(expect) = expect {
        input["expect_route"] = json!(expect);
    }
    input.to_string()
}

fn mailbox(dir: &std::path::Path) -> Mailbox {
    Mailbox::new(dir.join("agent-mailbox"))
}

fn never_called(_: &str, _: Value) -> Result<Value, WorkbenchError> {
    panic!("Manvi was called")
}

#[test]
fn a_preview_sends_nothing_and_a_confirm_sends_only_along_the_route_shown() {
    let dir = tempfile::tempdir().unwrap();
    let (state, root) = host(dir.path());
    prepare(&state, &root, "runs.prepare_terminal", SESSION, "claude");
    claim(&state, SESSION, None);
    let mailbox = mailbox(dir.path());

    let preview = send_with(
        &state,
        &ask(SESSION, "CI failed on build.", "r1", false, None),
        &mailbox,
        never_called,
    )
    .unwrap();
    assert_eq!(preview["route"], "mailbox");
    assert_eq!(preview["sent"], false);
    assert_eq!(preview["listening"], false);
    assert_eq!(preview["session_id"], SESSION);
    assert!(preview["reason"]
        .as_str()
        .unwrap()
        .contains("No GitPulse hook is waiting"));
    assert_eq!(
        mailbox.pending(SESSION),
        0,
        "a preview left a message behind"
    );

    // Confirm without naming the route the person saw is refused.
    let error = send_with(
        &state,
        &ask(SESSION, "CI failed on build.", "r1", true, None),
        &mailbox,
        never_called,
    )
    .unwrap_err();
    assert_eq!(error.code, "invalid_input");
    // And so is a route that is no longer the one shown.
    let error = send_with(
        &state,
        &ask(SESSION, "CI failed on build.", "r1", true, Some("resume")),
        &mailbox,
        never_called,
    )
    .unwrap_err();
    assert_eq!(error.code, "route_changed");
    // The message speaks of this press only: a different press may have
    // already sent something, and "nothing was sent" would deny it.
    assert!(
        error
            .message
            .contains("This press sent nothing; read the preview again."),
        "{}",
        error.message
    );
    assert!(
        !error.message.contains("Nothing was sent"),
        "{}",
        error.message
    );
    assert_eq!(
        mailbox.pending(SESSION),
        0,
        "a refused send left a message behind"
    );

    let sent = send_with(
        &state,
        &ask(SESSION, "CI failed on build.", "r1", true, Some("mailbox")),
        &mailbox,
        never_called,
    )
    .unwrap();
    assert_eq!(
        (sent["sent"].clone(), sent["status"].clone()),
        (json!(true), json!("queued"))
    );
    assert_eq!(mailbox.pending(SESSION), 1);
    let claimed = mailbox
        .claim_next(SESSION, SystemTime::now())
        .unwrap()
        .unwrap();
    assert_eq!(claimed.text, "CI failed on build.");

    // A retry of the same press writes nothing more and reports what became of it.
    let again = send_with(
        &state,
        &ask(SESSION, "CI failed on build.", "r1", true, Some("mailbox")),
        &mailbox,
        never_called,
    )
    .unwrap();
    assert_eq!(again["duplicate"], true);
    assert_eq!(again["status"], "picked_up");
    assert_eq!(mailbox.pending(SESSION), 0);
}

#[test]
fn text_that_cannot_be_sent_is_refused_in_the_preview_too() {
    let dir = tempfile::tempdir().unwrap();
    let (state, root) = host(dir.path());
    prepare(&state, &root, "runs.prepare_terminal", SESSION, "claude");
    claim(&state, SESSION, None);
    let mailbox = mailbox(dir.path());
    for (text, request) in [
        ("", "r"),
        ("   ", "r"),
        ("bell \u{7}", "r"),
        ("fine", "has space"),
        ("fine", ""),
    ] {
        let error = send_with(
            &state,
            &ask(SESSION, text, request, false, None),
            &mailbox,
            never_called,
        )
        .unwrap_err();
        assert_eq!(error.code, "invalid_input", "{text:?} {request:?}");
    }
    let too_long = "x".repeat(agent_mailbox::MAX_MESSAGE_BYTES + 1);
    assert_eq!(
        send_with(
            &state,
            &ask(SESSION, &too_long, "r", false, None),
            &mailbox,
            never_called
        )
        .unwrap_err()
        .code,
        "invalid_input"
    );
    for bad in [
        "{}",
        "null",
        r#"{"id":"x","text":"t","request_id":"r","skip":1}"#,
    ] {
        assert_eq!(
            send_with(&state, bad, &mailbox, never_called)
                .unwrap_err()
                .code,
            "invalid_input",
            "{bad}"
        );
    }
}

#[test]
fn an_attempt_that_never_started_cannot_be_sent_a_message() {
    let dir = tempfile::tempdir().unwrap();
    let (state, root) = host(dir.path());
    prepare(&state, &root, "runs.prepare_terminal", SESSION, "claude");
    let mailbox = mailbox(dir.path());
    let preview = send_with(
        &state,
        &ask(SESSION, "hello", "r1", false, None),
        &mailbox,
        never_called,
    )
    .unwrap();
    assert_eq!(preview["route"], "unavailable");
    let error = send_with(
        &state,
        &ask(SESSION, "hello", "r1", true, Some("unavailable")),
        &mailbox,
        never_called,
    )
    .unwrap_err();
    assert_eq!(error.code, "unavailable");
    assert_eq!(mailbox.pending(SESSION), 0);
}

#[test]
fn a_managed_follow_up_is_previewed_by_a_status_read_and_sent_with_the_confirmed_text() {
    let dir = tempfile::tempdir().unwrap();
    let (state, root) = host(dir.path());
    prepare(&state, &root, "runs.prepare_managed", "run", "claude");
    claim(&state, "run", Some("managed"));
    let mailbox = mailbox(dir.path());
    let mut calls: Vec<Value> = Vec::new();
    let mut manvi = |op: &str, params: Value| -> Result<Value, WorkbenchError> {
        assert_eq!(op, "work.runs.managed.continue");
        calls.push(params.clone());
        if params.get("message").is_some() {
            Ok(
                json!({"state": "delivered", "request_id": params["request_id"], "turn": 2, "max_turns": 3}),
            )
        } else {
            Ok(
                json!({"state": "parked", "turn": 1, "max_turns": 3, "idle_deadline": 1_700_000_600_i64}),
            )
        }
    };
    let preview = send_with(
        &state,
        &ask("run", "Fix the build.", "r1", false, None),
        &mailbox,
        &mut manvi,
    )
    .unwrap();
    assert_eq!(preview["route"], "managed");
    assert_eq!(preview["managed_state"], "parked");
    assert_eq!(preview["sent"], false);
    let sent = send_with(
        &state,
        &ask("run", "Fix the build.", "r1", true, Some("managed")),
        &mailbox,
        &mut manvi,
    )
    .unwrap();
    assert_eq!(
        (sent["sent"].clone(), sent["status"].clone()),
        (json!(true), json!("delivered"))
    );
    assert_eq!(sent["turn"], 2);
    assert_eq!(
        calls,
        vec![
            json!({"id": "run"}),
            json!({"id": "run"}),
            json!({"id": "run", "request_id": "r1", "message": "Fix the build."}),
        ]
    );
    assert_eq!(mailbox.pending(SESSION), 0);
}

#[test]
fn a_manvi_without_the_op_makes_the_route_unavailable_and_the_run_is_never_called_failed() {
    let dir = tempfile::tempdir().unwrap();
    let (state, root) = host(dir.path());
    prepare(&state, &root, "runs.prepare_managed", "run", "claude");
    claim(&state, "run", Some("managed"));
    let mailbox = mailbox(dir.path());
    let old = |_: &str, _: Value| -> Result<Value, WorkbenchError> {
        Err(WorkbenchError::new(
            "unsupported_operation",
            "This Manvi binary does not advertise work.runs.managed.continue.",
        ))
    };
    let preview = send_with(
        &state,
        &ask("run", "hello", "r1", false, None),
        &mailbox,
        old,
    )
    .unwrap();
    assert_eq!(preview["route"], "unavailable");
    assert!(preview["reason"]
        .as_str()
        .unwrap()
        .contains("This Manvi version cannot take follow-ups"));
    // The run's own state is untouched by having been asked.
    let row = state.request("runs.get", r#"{"id":"run"}"#).unwrap();
    assert_eq!(row["item"]["state"], "starting");
}

#[test]
fn a_refusal_after_the_preview_is_reported_as_not_sent_and_says_whether_to_retry() {
    let dir = tempfile::tempdir().unwrap();
    let (state, root) = host(dir.path());
    prepare(&state, &root, "runs.prepare_managed", "run", "claude");
    claim(&state, "run", Some("managed"));
    let mailbox = mailbox(dir.path());
    let busy = |_: &str, params: Value| -> Result<Value, WorkbenchError> {
        if params.get("message").is_some() {
            Err(WorkbenchError::new(
                "E_TURN_IN_FLIGHT",
                "a turn is still in flight",
            ))
        } else {
            Ok(json!({"state": "parked", "turn": 1, "max_turns": 3}))
        }
    };
    let sent = send_with(
        &state,
        &ask("run", "hello", "r1", true, Some("managed")),
        &mailbox,
        busy,
    )
    .unwrap();
    assert_eq!(
        (
            sent["sent"].clone(),
            sent["status"].clone(),
            sent["retryable"].clone()
        ),
        (json!(false), json!("refused"), json!(true))
    );
    assert_eq!(sent["code"], "E_TURN_IN_FLIGHT");
}

/// Sends one confirmed managed message to a Manvi that answers the status read
/// as parked and the message with `answer`.
fn confirmed_managed_send(answer: Result<Value, WorkbenchError>) -> Value {
    let dir = tempfile::tempdir().unwrap();
    let (state, root) = host(dir.path());
    prepare(&state, &root, "runs.prepare_managed", "run", "claude");
    claim(&state, "run", Some("managed"));
    let mailbox = mailbox(dir.path());
    let mut answer = Some(answer);
    send_with(
        &state,
        &ask("run", "hello", "r1", true, Some("managed")),
        &mailbox,
        |_: &str, params: Value| -> Result<Value, WorkbenchError> {
            if params.get("message").is_some() {
                answer.take().expect("one message is sent once")
            } else {
                Ok(json!({"state": "parked", "turn": 1, "max_turns": 3}))
            }
        },
    )
    .unwrap()
}

/// `E_CONTINUE_PENDING` is "the wait for the outcome ran out": the message may
/// have been written. It was reported as `sent: false`, `refused`, which the
/// panel read as "Nothing was sent", after which an edit made a second message.
#[test]
fn a_pending_outcome_is_unknown_and_never_reported_as_not_sent() {
    let sent = confirmed_managed_send(Err(WorkbenchError::new(
        "E_CONTINUE_PENDING",
        "the wait ran out",
    )));
    assert_eq!(sent["sent"], Value::Null, "{sent}");
    assert_eq!(sent["status"], "pending");
    assert_eq!(sent["retryable"], true);
    assert_eq!(sent["code"], "E_CONTINUE_PENDING");
    let reason = sent["reason"].as_str().unwrap();
    assert!(reason.contains("same request id"), "{reason}");
    assert!(
        !reason.to_lowercase().contains("nothing was sent"),
        "{reason}"
    );
}

#[test]
fn an_unresolved_delivery_is_unknown_and_not_retryable() {
    let sent = confirmed_managed_send(Err(WorkbenchError::new(
        "E_CONTINUE_UNRESOLVED",
        "cannot tell it from a lost one",
    )));
    assert_eq!(sent["sent"], Value::Null, "{sent}");
    assert_eq!(sent["status"], "unresolved");
    assert_eq!(sent["retryable"], false);
    assert!(sent["reason"]
        .as_str()
        .unwrap()
        .contains("will not be sent again"));
}

#[test]
fn a_code_this_build_does_not_know_is_unconfirmed_and_not_a_refusal() {
    let sent = confirmed_managed_send(Err(WorkbenchError::new(
        "E_SOMETHING_NEW",
        "a code from a newer Manvi",
    )));
    assert_eq!(sent["sent"], Value::Null, "{sent}");
    assert_eq!(sent["status"], "unconfirmed");
    assert_eq!(sent["retryable"], false);
}

/// A message answered with the run's state, not a delivery, says nothing about
/// whether it was written; the old reason said "Delivered" for an `ended` run.
#[test]
fn a_message_answered_with_a_run_state_is_not_called_delivered() {
    for state in ["parked", "running", "ended"] {
        let sent = confirmed_managed_send(Ok(json!({"state": state, "turn": 1, "max_turns": 3})));
        assert_eq!(sent["sent"], Value::Null, "{state}: {sent}");
        assert_eq!(sent["status"], "unconfirmed", "{state}");
        let reason = sent["reason"].as_str().unwrap();
        assert!(!reason.contains("Delivered"), "{state}: {reason}");
        assert!(reason.contains("not known"), "{state}: {reason}");
    }
    // An answered `unresolved` state is the unresolved delivery, in words.
    let sent = confirmed_managed_send(Ok(
        json!({"state": "unresolved", "turn": 1, "max_turns": 3}),
    ));
    assert_eq!(sent["sent"], Value::Null, "{sent}");
    assert_eq!(sent["status"], "unresolved");
    let reason = sent["reason"].as_str().unwrap();
    assert!(reason.contains("will not be sent again"), "{reason}");
}

#[test]
fn a_documented_refusal_is_still_a_definite_not_sent() {
    for code in [
        "E_TURN_IN_FLIGHT",
        "E_CONTINUE_RATE",
        "E_CONTINUE_LIMIT",
        "E_RUN_ENDED",
        "E_DEPENDENCY",
    ] {
        let sent = confirmed_managed_send(Err(WorkbenchError::new(code, "refused")));
        assert_eq!(
            (sent["sent"].clone(), sent["status"].clone()),
            (json!(false), json!("refused")),
            "{code}"
        );
    }
}

/// The preview of the not-listening mailbox route says only what GitPulse
/// observed: no waiting hook. It names no cause it did not see.
#[test]
fn the_not_listening_preview_names_no_unobserved_cause_as_the_cause() {
    let preview = Route::Mailbox {
        session: SESSION.into(),
        listening: false,
    }
    .preview();
    let reason = preview["reason"].as_str().unwrap();
    assert!(reason.contains("cannot see why"), "{reason}");
    assert!(reason.contains("hook build without deliver"), "{reason}");
    assert!(
        !reason.contains("in the middle of a turn, or has been idle"),
        "{reason}"
    );
}
