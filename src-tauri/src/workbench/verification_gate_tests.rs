//! The evidence gate on an agent's `done`: one failing-first test per way a
//! claim can lack evidence, plus the paths that must keep working.

use super::tests::{filed, git_repo, live, person_edits, profile};
use super::{complete_task, resend};
use crate::workbench::verification_gate::{current_head, EVIDENCE_TYPE};
use dc_store::Store;
use serde_json::{json, Value};

/// A repository with one commit, so it has a head to verify.
fn committed_repo() -> tempfile::TempDir {
    let repo = git_repo();
    crate::test_support::git_in(
        repo.path(),
        &["commit", "-q", "--allow-empty", "-m", "init"],
    );
    repo
}

pub(crate) fn head_of(repo: &tempfile::TempDir) -> String {
    current_head(repo.path().to_str().unwrap()).unwrap()
}

/// What Manvi's `work.verify.task` stores, as the sidecar would write it.
pub(crate) fn record(
    item: &str,
    outcome: &str,
    state: &str,
    repo: &tempfile::TempDir,
    head: &str,
) -> Value {
    json!({
        "request_id": "req-1", "item_id": item, "state": state, "outcome": outcome,
        "worktree": repo.path().to_str().unwrap(), "head": head,
        "criteria": [
            {"index": 1, "text": "exports csv", "status": if outcome == "passed" { "met" } else { "unmet" },
             "findings": if outcome == "passed" { json!([]) } else { json!(["no csv writer found"]) }}
        ],
        "degraded": if outcome == "degraded" { json!(["the critic did not complete"]) } else { json!([]) },
    })
}

pub(crate) fn store_record(store: &Store, item: &str, record: &Value) {
    store
        .evidence_append(EVIDENCE_TYPE, Some(item), None, None, &record.to_string())
        .unwrap();
}

/// A passed record for the repository's current head, for tests of other
/// behaviour that need a task an agent may finish.
pub(crate) fn verified(store: &Store, repo: &tempfile::TempDir, item: &str) {
    let head = head_of(repo);
    store_record(store, item, &record(item, "passed", "settled", repo, &head));
}

fn claim(store: &Store, repo: &tempfile::TempDir, id: &str, summary: Option<&str>) -> Value {
    complete_task(
        store,
        repo.path().to_str().unwrap(),
        id,
        "done",
        None,
        summary,
        None,
    )
    .unwrap()
}

fn logs(store: &Store, id: &str) -> String {
    live(store, id).unwrap()["logs"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

fn setup() -> (tempfile::TempDir, Store, tempfile::TempDir, String) {
    let (dir, path) = profile();
    let store = Store::open(&path).unwrap();
    let repo = committed_repo();
    let id = filed(&store, &repo, "gp-gate", "Ship the exporter");
    (dir, store, repo, id)
}

fn assert_held(store: &Store, id: &str, response: &Value, reason: &str) {
    assert_eq!(response["status"], "review", "{response}");
    assert_eq!(response["requested_status"], "done");
    assert_eq!(response["held_for_review"], true);
    assert!(
        response["verification"].as_str().unwrap().contains(reason),
        "{response}"
    );
    assert_eq!(live(store, id).unwrap()["status"], "review");
    let history = logs(store, id);
    assert!(history.contains("held in review"), "{history}");
    assert!(history.contains(reason), "{history}");
}

#[test]
fn an_agent_claiming_done_with_no_verification_record_lands_in_review() {
    let (_d, store, repo, id) = setup();
    let response = claim(&store, &repo, &id, Some("All done"));
    assert_held(&store, &id, &response, "no verification record");
    assert!(
        logs(&store, &id).trim_end().ends_with("---\nAll done"),
        "the agent's summary is still recorded last"
    );
}

#[test]
fn a_degraded_record_does_not_let_an_agent_finish() {
    let (_d, store, repo, id) = setup();
    let head = head_of(&repo);
    store_record(
        &store,
        &id,
        &record(&id, "degraded", "settled", &repo, &head),
    );
    let response = claim(&store, &repo, &id, None);
    assert_held(&store, &id, &response, "outcome \"degraded\"");
    assert!(logs(&store, &id).contains("the critic did not complete"));
}

#[test]
fn a_failed_record_does_not_let_an_agent_finish_and_names_the_unmet_criterion() {
    let (_d, store, repo, id) = setup();
    let head = head_of(&repo);
    store_record(&store, &id, &record(&id, "failed", "settled", &repo, &head));
    let response = claim(&store, &repo, &id, None);
    assert_held(&store, &id, &response, "outcome \"failed\"");
    assert!(logs(&store, &id).contains("no csv writer found"));
}

#[test]
fn a_verification_still_running_is_not_a_verdict() {
    let (_d, store, repo, id) = setup();
    let head = head_of(&repo);
    store_record(&store, &id, &record(&id, "", "running", &repo, &head));
    let response = claim(&store, &repo, &id, None);
    assert_held(&store, &id, &response, "still \"running\"");
}

#[test]
fn a_passed_record_for_an_older_head_is_stale() {
    let (_d, store, repo, id) = setup();
    verified(&store, &repo, &id);
    crate::test_support::git_in(
        repo.path(),
        &["commit", "-q", "--allow-empty", "-m", "more work"],
    );
    let response = claim(&store, &repo, &id, None);
    assert_held(
        &store,
        &id,
        &response,
        "the work changed after it was verified",
    );
}

#[test]
fn a_record_for_a_different_item_is_not_evidence_for_this_one() {
    let (_d, store, repo, id) = setup();
    let head = head_of(&repo);
    store_record(
        &store,
        &id,
        &record("some-other-item", "passed", "settled", &repo, &head),
    );
    let response = claim(&store, &repo, &id, None);
    assert_held(&store, &id, &response, "for another task");
}

#[test]
fn a_passed_record_for_another_repositorys_worktree_is_refused() {
    let (_d, store, repo, id) = setup();
    let other = committed_repo();
    let head = head_of(&other);
    store_record(
        &store,
        &id,
        &record(&id, "passed", "settled", &other, &head),
    );
    let response = claim(&store, &repo, &id, None);
    assert_held(&store, &id, &response, "belongs to another repository");
}

#[test]
fn a_newer_unreadable_record_is_not_shadowed_by_an_older_pass() {
    let (_d, store, repo, id) = setup();
    verified(&store, &repo, &id);
    store
        .evidence_append(EVIDENCE_TYPE, Some(&id), None, None, "{\"request_id\": 7}")
        .unwrap();
    let response = claim(&store, &repo, &id, None);
    assert_held(&store, &id, &response, "could not be parsed");
}

#[test]
fn an_unreadable_evidence_store_fails_closed_to_review() {
    let (_d, path) = profile();
    let store = Store::open(&path).unwrap();
    let repo = committed_repo();
    let id = filed(&store, &repo, "gp-gate", "Ship the exporter");
    verified(&store, &repo, &id);
    // Another connection takes the evidence table away from under the store.
    let other = rusqlite::Connection::open(&path).unwrap();
    other
        .execute_batch("ALTER TABLE evidence RENAME TO evidence_gone")
        .unwrap();
    let response = claim(&store, &repo, &id, None);
    assert_held(&store, &id, &response, "could not be read");
}

#[test]
fn a_passed_record_for_the_current_head_lets_the_agent_finish_and_is_attached() {
    let (_d, store, repo, id) = setup();
    verified(&store, &repo, &id);
    let response = claim(&store, &repo, &id, Some("Exporter shipped"));
    assert_eq!(response["status"], "done", "{response}");
    assert_eq!(response["held_for_review"], false);
    assert!(response["verification"]
        .as_str()
        .unwrap()
        .contains("Verification passed"));
    let history = logs(&store, &id);
    assert!(history.contains("--- Verification passed ("), "{history}");
    assert!(
        history.trim_end().ends_with("---\nExporter shipped"),
        "{history}"
    );
}

#[test]
fn retrying_a_held_or_passed_claim_writes_nothing_twice() {
    let (_d, store, repo, id) = setup();
    let first = claim(&store, &repo, &id, Some("All done"));
    assert_eq!(first["outcome"], "updated");
    let held_logs = logs(&store, &id);
    let again = claim(&store, &repo, &id, Some("All done"));
    assert_eq!(again["outcome"], "unchanged", "{again}");
    assert_eq!(again["status"], "review");
    assert_eq!(logs(&store, &id), held_logs);

    // Once verified, the same claim finishes it, and a retry of that is a no-op.
    verified(&store, &repo, &id);
    let done = claim(&store, &repo, &id, Some("All done"));
    assert_eq!(done["status"], "done", "{done}");
    let done_logs = logs(&store, &id);
    let retry = claim(&store, &repo, &id, Some("All done"));
    assert_eq!(retry["outcome"], "unchanged", "{retry}");
    assert_eq!(logs(&store, &id), done_logs);
}

#[test]
fn a_person_moving_the_card_to_done_is_not_gated() {
    let (_d, store, _repo, id) = setup();
    let moved = person_edits(&store, &id, |input| {
        input.insert("status".into(), json!("done"));
    });
    assert_eq!(moved["status"], "done");
    let item = live(&store, &id).unwrap();
    let again = resend(&item, &id, item["revision"].as_i64().unwrap(), "person");
    assert_eq!(again["status"], "done");
}

#[test]
fn concurrent_claims_never_land_unverified_work_in_done() {
    let (_d, path) = profile();
    let repo = committed_repo();
    let id = {
        let store = Store::open(&path).unwrap();
        filed(&store, &repo, "gp-gate", "Ship the exporter")
    };
    let repo_path = repo.path().to_str().unwrap().to_owned();
    let handles: Vec<_> = (0..4)
        .map(|n| {
            let (path, repo_path, id) = (path.clone(), repo_path.clone(), id.clone());
            std::thread::spawn(move || {
                let store = Store::open(&path).unwrap();
                complete_task(
                    &store,
                    &repo_path,
                    &id,
                    "done",
                    None,
                    Some(&format!("claim {n}")),
                    None,
                )
            })
        })
        .collect();
    for handle in handles {
        // A lost race is reported as a conflict, never as a move to done.
        if let Ok(response) = handle.join().unwrap() {
            assert_eq!(response["status"], "review", "{response}");
        }
    }
    let store = Store::open(&path).unwrap();
    assert_eq!(live(&store, &id).unwrap()["status"], "review");
}

#[test]
fn a_start_request_names_only_the_work_and_defaults_to_the_worktrees_head() {
    let repo = committed_repo();
    let head = head_of(&repo);
    let input = json!({"id": "item-1", "request_id": "req-1", "worktree": repo.path().to_str().unwrap(), "base": "main"});
    let params = super::super::verification_gate::start_params(&input.to_string()).unwrap();
    assert_eq!(params["item_id"], "item-1");
    assert_eq!(params["head"], head);
    assert_eq!(params["base"], "main");
    assert_eq!(params.as_object().unwrap().len(), 5, "{params}");
}

#[test]
fn a_start_request_cannot_carry_a_command_or_a_bad_identity() {
    use super::super::verification_gate::{get_params, start_params};
    let good = json!({"id": "item-1", "request_id": "req-1", "worktree": "/tmp", "base": "main", "head": "abc"});
    assert!(start_params(&good.to_string()).is_ok());
    for extra in [("command", json!("rm -rf /")), ("criteria", json!(["x"]))] {
        let mut input = good.clone();
        input[extra.0] = extra.1;
        assert_eq!(
            start_params(&input.to_string()).unwrap_err().code,
            "invalid_input"
        );
    }
    for bad in [
        json!({"id": "a b"}),
        json!({"request_id": ""}),
        json!({"base": " "}),
    ] {
        let mut input = good.clone();
        input
            .as_object_mut()
            .unwrap()
            .extend(bad.as_object().unwrap().clone());
        assert_eq!(
            start_params(&input.to_string()).unwrap_err().code,
            "invalid_input",
            "{input}"
        );
    }
    assert_eq!(start_params("[]").unwrap_err().code, "invalid_input");
    assert_eq!(
        get_params(r#"{"id":"item-1","request_id":"x y"}"#)
            .unwrap_err()
            .code,
        "invalid_input"
    );
    assert_eq!(
        get_params(r#"{"id":"item-1"}"#).unwrap(),
        json!({"item_id": "item-1"})
    );
}
