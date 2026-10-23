//! Adversarial probes (W6-L2 reviewer) for `session_brief`'s new "readable"
//! branch and for LedgerStatus serde. Tests named `BREAK_*` are expected to FAIL
//! on the reviewed head: each asserts what an honest brief must say.

use crate::insights::InsightsSnapshot;

fn real_snapshot_with_one_event() -> (tempfile::TempDir, InsightsSnapshot) {
    let dir = tempfile::tempdir().expect("tempdir");
    crate::test_support::git_in(dir.path(), &["init", "-q"]);
    crate::test_support::trust_repo(dir.path());
    let repo = dir.path().canonicalize().expect("canonical");
    let repo = repo.to_string_lossy().into_owned();
    crate::ledger::append(crate::ledger::Draft {
        repo_path: repo.clone(),
        action: "git.commit".to_string(),
        actor_kind: Some(crate::ledger::ActorKind::Human),
        outcome: Some(crate::ledger::Outcome::Ok),
        ..Default::default()
    })
    .expect("append");
    let snapshot = crate::insights::snapshot(&repo);
    (dir, snapshot)
}

fn ledger_line(brief: &str) -> String {
    brief
        .lines()
        .find(|l| l.starts_with("ledger:"))
        .unwrap_or("<no ledger line>")
        .to_string()
}

/// A real read-only snapshot of a ledger that holds an event but has never had a
/// catch-up pass (`last_catch_up_basis: newest_row`, `catch_up: null`) is not
/// stale, and the brief says "a catch-up ran recently". No catch-up ran: the
/// freshness came from the newest event row.
#[test]
fn break_brief_claims_a_catch_up_ran_when_only_an_event_row_is_recent() {
    let (_dir, snapshot) = real_snapshot_with_one_event();
    assert_eq!(snapshot.ledger.recording_state, "readable", "{:?}", snapshot.ledger);
    assert_eq!(snapshot.ledger.freshness.last_catch_up_basis, "newest_row");
    assert!(snapshot.ledger.freshness.catch_up.is_none());
    let line = ledger_line(&crate::hooks::session_brief(&snapshot, "/repo", "startup"));
    assert!(
        !line.contains("a catch-up ran recently"),
        "brief asserts a catch-up that never ran: {line}"
    );
}

/// The readable branch ignores `error`: the legacy-unconsolidated caveat (rows a
/// read cannot see) and the history-only-bindings notice never reach the brief,
/// which says only "readable (...; a catch-up ran recently)".
#[test]
fn break_readable_brief_hides_the_legacy_unconsolidated_caveat() {
    let (_dir, mut snapshot) = real_snapshot_with_one_event();
    snapshot.ledger.error = "legacy worktree ledgers are not consolidated; rows from siblings are not shown".to_string();
    snapshot.ledger.error_code = "legacy_ledger_unconsolidated".to_string();
    let line = ledger_line(&crate::hooks::session_brief(&snapshot, "/repo", "startup"));
    assert!(
        line.contains("not consolidated"),
        "a ledger that cannot show sibling rows is briefed with no caveat: {line}"
    );
}

#[test]
fn break_readable_brief_hides_the_history_only_bindings_notice() {
    let (_dir, mut snapshot) = real_snapshot_with_one_event();
    snapshot.ledger.history_only_bindings = 2;
    snapshot.ledger.error = "2 bind/unbind row(s) written by sibling worktrees were kept as history only".to_string();
    snapshot.ledger.error_code = "binding_rows_history_only".to_string();
    let line = ledger_line(&crate::hooks::session_brief(&snapshot, "/repo", "startup"));
    assert!(
        line.contains("history only") || line.contains("bind"),
        "history-only bindings are briefed with no notice: {line}"
    );
}

/// The old brief said "recording (N append(s) dropped - history incomplete)"
/// whenever dropped > 0; the readable branch drops that warning entirely.
#[test]
fn break_readable_brief_hides_dropped_appends() {
    let (_dir, mut snapshot) = real_snapshot_with_one_event();
    snapshot.ledger.dropped = 3;
    let line = ledger_line(&crate::hooks::session_brief(&snapshot, "/repo", "startup"));
    assert!(
        line.contains("dropped") && line.contains('3'),
        "3 dropped appends vanished from the brief: {line}"
    );
}

/// A real snapshot can no longer say "recording" at all: `recording` is false on
/// every read-only snapshot, so the `if snapshot.ledger.recording` branch of the
/// brief is unreachable from `insights::snapshot`.
#[test]
fn a_real_snapshot_never_reaches_the_recording_branch() {
    let (_dir, snapshot) = real_snapshot_with_one_event();
    assert!(!snapshot.ledger.recording);
    let line = ledger_line(&crate::hooks::session_brief(&snapshot, "/repo", "startup"));
    assert!(line.starts_with("ledger: readable"), "{line}");
}

/// Every recording_state x stale x error x recording combination: none may brief
/// a ledger that carries an error as plain "readable" with the error absent, and
/// "readable" must never appear for state unknown.
#[test]
fn brief_matrix_never_says_readable_for_unknown_and_never_drops_a_hard_error() {
    let (_dir, base) = real_snapshot_with_one_event();
    let mut hidden = Vec::new();
    for recording in [false, true] {
        for state in ["recording", "readable", "unknown"] {
            for stale in [false, true] {
                for error in ["", "database is locked"] {
                    let mut snapshot = base.clone();
                    snapshot.ledger.recording = recording;
                    snapshot.ledger.recording_state = state.to_string();
                    snapshot.ledger.freshness.stale = stale;
                    snapshot.ledger.freshness.stale_reason =
                        if stale { "no catch-up newer than 600s".to_string() } else { String::new() };
                    snapshot.ledger.error = error.to_string();
                    let line = ledger_line(&crate::hooks::session_brief(&snapshot, "/repo", "startup"));
                    if state == "unknown" {
                        assert!(!line.contains("readable"), "unknown briefed as readable: {line}");
                    }
                    if !error.is_empty() && !line.contains(error) {
                        hidden.push(format!("recording={recording} state={state} stale={stale} -> {line}"));
                    }
                }
            }
        }
    }
    assert!(hidden.is_empty(), "an error vanished from the brief:\n{}", hidden.join("\n"));
}

/// Stored or cached LedgerStatus JSON from before the fold (no freshness, no
/// recording_state) still deserialises, and a stale-default is what it gets.
#[test]
fn an_older_stored_ledger_status_still_deserialises() {
    let old = serde_json::json!({
        "recording": true, "path": "/r/.devcouncil/ledger.sqlite", "dropped": 0,
        "error": "", "error_code": ""
    });
    let status: crate::ledger::LedgerStatus = serde_json::from_value(old).expect("old json");
    assert!(status.recording);
    assert_eq!(status.dropped_scope, "this_process");

    let with_unknown_field = serde_json::json!({
        "recording": false, "path": "", "dropped": 0, "error": "x", "error_code": "y",
        "some_future_field": 1
    });
    serde_json::from_value::<crate::ledger::LedgerStatus>(with_unknown_field).expect("unknown field");
}

/// The creating `status()` serialises `recording_state: recording` next to a
/// `freshness_ok: false` / `stale: true` block. Nothing reads it today (rg over
/// src/ and src-tauri/src finds no consumer), but the IPC payload of a healthy
/// recording ledger now claims stale: pin that this is only the documented default.
#[test]
fn the_creating_status_of_a_healthy_ledger_carries_the_not_read_default() {
    let dir = tempfile::tempdir().expect("tempdir");
    crate::test_support::git_in(dir.path(), &["init", "-q"]);
    crate::test_support::trust_repo(dir.path());
    let repo = dir.path().canonicalize().expect("canonical").to_string_lossy().into_owned();
    let status = crate::ledger::status(&repo);
    assert!(status.recording, "{status:?}");
    let wire = serde_json::to_value(&status).expect("wire");
    assert_eq!(wire["recording_state"], "recording");
    assert_eq!(wire["stale"], true);
    assert_eq!(wire["freshness_ok"], false);
    assert_eq!(wire["freshness_error"], "freshness was not read for this answer");
}
